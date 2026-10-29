//! Ports of the reference suite: offline Microdata/RDFa extraction and validation.
//! Each test names its reference source. Cases that read reference-only internals (`_extract_microdata`,
//! `_extract_rdfa`, `_rdfa_term`) are listed in the slice report instead of being ported.
//! the reference `check_schema(url=..., html=...)` has no Rust equivalent on the HTML path, so those
//! cases feed the HTML through a fake fetcher on the `Url` path (no network).

use serde_json::{json, Value};
use spiderhead_parse::parser::{FetchResponse, HtmlFetcher};
use spiderhead_parse::schema_org::{
    check_schema, normalize_jsonld_ids, rich_results_rules, SchemaCheck, SchemaSource,
    MAX_HTML_BYTES,
};

const RDFA_GRAPH: &str = r##"<div vocab="https://schema.org/" prefix="xsd: http://www.w3.org/2001/XMLSchema#"
     typeof="Person" about="#ada">
  <span property="name" lang="en">Ada</span>
  <meta property="birthDate" content="1990-01-01" datatype="xsd:date">
  <span property="additionalName" inlist>Queen</span>
  <span property="additionalName" inlist>of coding</span>
  <a rel="knows" href="#grace">Grace</a>
  <div about="#grace" typeof="Person">
    <span property="name">Grace</span>
    <span rev="parent" resource="#ada"></span>
  </div>
</div>
"##;

const RDFA_CONTEXT_EDGES: &str = r##"<div about="[_:ada]" typeof="schema:Person" property="schema:name"
     content="Ada" lang="en">
  <span property="schema:alternateName" datatype="" lang="" href="#literal">Plain name</span>
  <meta property="schema:birthDate" content="1990-01-01" datatype="xsd:date" lang="fr">
  <a rel="canonical nofollow" href="/canonical">Canonical page</a>
  <a rel="schema:knows" resource="[_:friend]">
    <span property="schema:name">Grace</span>
  </a>
</div>
<div about="[_:friend]" typeof="schema:Person"></div>
"##;

const RDFA_COPY_GRAPH: &str = r##"<div vocab="https://schema.org/">
  <div resource="#direct" typeof="rdfa:Pattern">
    <span property="name">Shared name</span>
  </div>
  <div resource="#base" typeof="rdfa:Pattern">
    <span property="description">Shared description</span>
  </div>
  <div resource="#chained" typeof="rdfa:Pattern">
    <span property="name">Chained name</span>
    <link property="rdfa:copy" href="#base">
  </div>
  <div resource="#cycle-a" typeof="rdfa:Pattern">
    <span property="name">Cycle name</span>
    <link property="rdfa:copy" href="#cycle-b">
  </div>
  <div resource="#cycle-b" typeof="rdfa:Pattern">
    <span property="description">Cycle description</span>
    <link property="rdfa:copy" href="#cycle-a">
  </div>
  <div resource="#event" typeof="schema:Thing">
    <link property="rdfa:copy" href="#direct">
  </div>
  <div resource="#article" typeof="schema:Thing">
    <link property="rdfa:copy" href="#chained">
  </div>
  <div resource="#other" typeof="schema:Thing">
    <link property="rdfa:copy" href="#cycle-a">
  </div>
</div>
"##;

/// Serves one fixed HTML body for any URL; `final_url` is the requested URL.
struct Fixed(String);

impl HtmlFetcher for Fixed {
    fn get(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _timeout: f64,
        _max_redirects: u32,
    ) -> std::result::Result<FetchResponse, String> {
        Ok(FetchResponse {
            final_url: url.to_owned(),
            status_code: 200,
            text: self.0.clone(),
            headers: Vec::new(),
        })
    }
}

fn to_json(check: SchemaCheck) -> Value {
    serde_json::to_value(check).unwrap()
}

fn check(html: &str) -> Value {
    to_json(check_schema(SchemaSource::Html(html), None, 0.0, None).unwrap())
}

fn check_in(html: &str, base: &str) -> Value {
    to_json(check_schema(SchemaSource::Html(html), None, 0.0, Some(base)).unwrap())
}

fn check_url(url: &str, html: &str) -> Value {
    let fetcher = Fixed(html.to_owned());
    to_json(check_schema(SchemaSource::Url(url), Some(&fetcher), 0.0, None).unwrap())
}

fn items(value: &Value) -> &[Value] {
    value.as_array().expect("array")
}

fn is_empty(value: &Value) -> bool {
    items(value).is_empty()
}

fn rules_version() -> Value {
    rich_results_rules()["version"].clone()
}

fn contains_str(value: &Value, needle: &str) -> bool {
    value.as_str().is_some_and(|s| s.contains(needle))
}

#[test]
fn test_microdata_nested_scope_is_a_property_not_a_leaked_parent_property() {
    let r = check(
        r##"
        <div itemscope itemtype="https://schema.org/Product">
          <span itemprop="name">Widget</span>
          <div itemprop="offers" itemscope itemtype="https://schema.org/Offer">
            <meta itemprop="price" content="19.99">
            <meta itemprop="priceCurrency" content="USD">
          </div>
        </div>"##,
    );
    assert_eq!(r["blocks"], 0);
    assert_eq!(r["markup_counts"], json!({"microdata": 2, "rdfa": 0}));
    let entities = items(&r["entities"]);
    assert_eq!(entities.len(), 2);
    assert_eq!(entities[0]["types"], json!(["Product"]));
    assert_eq!(entities[0]["format"], "microdata");
    assert_eq!(entities[1]["types"], json!(["Offer"]));
    assert_eq!(entities[1]["format"], "microdata");
    assert!(entities.iter().all(|e| is_empty(&e["errors"])));
    assert_eq!(r["graph"]["top_level"], 1);
    let rich = &items(&r["rich_results"])[0];
    assert_eq!(rich["eligible"], true);
    assert_eq!(rich["rules_version"], rules_version());
    assert_eq!(rich["format"], "microdata");
}

#[test]
fn test_microdata_missing_itemref_and_wrong_type_are_actionable() {
    let r = check(
        r##"
        <div itemscope itemtype="https://schema.org/Article" itemref="missing">
          <span itemprop="servesCuisine">Pizza</span><span itemprop="bogus">X</span>
        </div>"##,
    );
    let errors = items(&r["entities"][0]["errors"]);
    assert!(errors
        .iter()
        .any(|e| contains_str(e, "itemref target missing")));
    assert!(errors
        .iter()
        .any(|e| contains_str(e, "servesCuisine") && contains_str(e, "Article")));
    assert!(errors.iter().any(|e| contains_str(e, "Property bogus")));
    assert!(items(&r["findings"])
        .iter()
        .any(|f| contains_str(f, "Vocabulary errors: 3")));
}

#[test]
fn test_foreign_vocabulary_is_not_scored_as_a_schema_rich_result() {
    let markups = [
        r##"<div itemscope itemtype="https://example.org/Product"><span itemprop="name">X</span></div>"##,
        r##"<div vocab="https://example.org/" typeof="Product"><span property="name">X</span></div>"##,
        r##"<script type="application/ld+json">{"@context":"https://example.org/","@type":"Product","name":"X"}</script>"##,
    ];
    for markup in markups {
        let r = check(markup);
        assert!(is_empty(&r["rich_results"]), "{markup}");
        assert!(!is_empty(&r["entities"][0]["warnings"]), "{markup}");
        assert!(is_empty(&r["entities"][0]["errors"]), "{markup}");
        assert!(items(&r["vocabularies"])
            .iter()
            .any(|v| v["supported"] == false));
    }
}

#[test]
fn test_rdfa_vocabulary_prefix_and_full_iri() {
    let cases = [
        (r##"vocab="https://schema.org/""##, "Article", "headline"),
        (r##"vocab="https://schema.org""##, "Article", "headline"),
        (
            r##"prefix="s: https://schema.org/""##,
            "s:Article",
            "s:headline",
        ),
        (
            r##"xmlns:s="https://schema.org/""##,
            "s:Article",
            "s:headline",
        ),
        (
            "",
            "https://schema.org/Article",
            "https://schema.org/headline",
        ),
    ];
    for (context, type_name, prop) in cases {
        let r = check(&format!(
            r##"
        <div {context} typeof="{type_name}" about="#article">
          <span property="{prop}" content="Title">Visible title</span>
        </div>"##
        ));
        let label = format!("{context} {type_name} {prop}");
        assert_eq!(r["entities"][0]["types"], json!(["Article"]), "{label}");
        assert_eq!(r["entities"][0]["format"], "rdfa", "{label}");
        assert!(is_empty(&r["entities"][0]["errors"]), "{label}");
        assert_eq!(r["markup_coverage"][1]["status"], "checked", "{label}");
        assert!(!items(&r["rich_results"][0]["missing_recommended"])
            .iter()
            .any(|m| m == "headline"));
    }
}

#[test]
fn test_rdfa_nested_entity_and_scoped_vocabulary() {
    let r = check(
        r##"
        <div vocab="https://schema.org/" typeof="Product">
          <span property="name">Widget</span>
          <div property="offers" typeof="Offer">
            <meta property="price" content="19.99"><meta property="priceCurrency" content="USD">
          </div>
        </div>"##,
    );
    let entities = items(&r["entities"]);
    assert_eq!(entities[0]["types"], json!(["Product"]));
    assert_eq!(entities[1]["types"], json!(["Offer"]));
    assert!(entities
        .iter()
        .all(|e| is_empty(&e["errors"]) && is_empty(&e["warnings"])));
    assert_eq!(r["graph"]["top_level"], 1);
    assert_eq!(items(&r["rich_results"])[0]["eligible"], true);
}

#[test]
fn test_rdfa_relation_is_a_validated_graph_edge() {
    let r = check(
        r##"
        <div vocab="https://schema.org/" typeof="Product">
          <span property="name">Widget</span><a rel="offers" href="#offer">Offer</a>
          <div about="#offer" typeof="Offer">
            <meta property="price" content="19.99"><meta property="priceCurrency" content="USD">
          </div>
        </div>"##,
    );
    assert_eq!(r["markup_coverage"][1]["status"], "checked");
    assert!(is_empty(&r["entities"][0]["errors"]));
    assert_eq!(r["graph"]["linked_by_id"], 1);
}

#[test]
fn test_rdfa_incomplete_relation_is_explicitly_partial() {
    let r = check(
        r##"<div vocab="https://schema.org/" typeof="Person"><span rel="knows"></span></div>"##,
    );
    assert_eq!(r["markup_coverage"][1]["status"], "partial");
    assert!(items(&r["markup_coverage"][1]["reasons"])
        .iter()
        .any(|reason| contains_str(reason, "incomplete rel/rev")));
}

#[test]
fn test_rdfa_orphan_schema_property_and_foreign_metadata_are_distinct() {
    let orphan = check(r##"<span property="schema:name">X</span>"##);
    assert_eq!(orphan["markup_coverage"][1]["status"], "checked");
    assert!(items(&orphan["entities"][0]["errors"])
        .iter()
        .any(|e| contains_str(e, "Missing typeof")));
    let og = check(r##"<meta property="og:title" content="Title">"##);
    assert_eq!(og["markup_coverage"][1]["status"], "absent");
}

// Public half only: the node-level asserts read the reference `_extract_rdfa` graph (see the report).
#[test]
fn test_rdfa_subject_graph_lists_typed_literals_and_reverse_edges() {
    let markup = RDFA_GRAPH;
    let base = "https://example.test/article/";
    let r = check_in(markup, base);
    assert_eq!(r["markup_coverage"][1]["status"], "checked");
    // Both RDF predicates identify Grace, so the distinct referenced-ID count is one.
    assert_eq!(r["graph"]["linked_by_id"], 1);
    let person = items(&r["entities"])
        .iter()
        .find(|e| e["types"] == json!(["Person"]) && contains_str(&e["id"], "#ada"))
        .expect("Ada entity");
    assert!(is_empty(&person["errors"]));
    assert!(is_empty(&person["warnings"]));
    assert_eq!(r["rules_version"], rules_version());
    assert!(r["markup_counts"]["rdfa"].as_u64().unwrap() > 0);
}

#[test]
fn test_schema_validation_marks_oversized_html_as_partial() {
    let html = " ".repeat(MAX_HTML_BYTES + 1);
    let r = check(&html);
    assert!(items(&r["markup_coverage"])
        .iter()
        .all(|item| item["status"] == "partial"));
    assert!(contains_str(
        &items(&r["findings"])[0],
        "byte schema validation limit"
    ));
}

// Public half only: the node-level asserts read the reference `_extract_rdfa` (see the report).
#[test]
fn test_rdfa_initial_context_safe_curie_subjects_and_literal_precedence() {
    let markup = RDFA_CONTEXT_EDGES;
    let r = check_in(markup, "https://example.test/article/");
    assert_eq!(r["markup_coverage"][1]["status"], "checked");
    assert!(!items(&r["findings"])
        .iter()
        .any(|f| contains_str(f, "canonical") || contains_str(f, "nofollow")));
}

#[test]
fn test_rdfa_foreign_vocabulary_does_not_poison_schema_siblings() {
    let r = check(
        r##"
        <div vocab="https://foreign.example/vocab/" typeof="Thing">
          <span property="name">Foreign</span>
        </div>
        <div typeof="schema:Person"><span property="schema:name">Ada</span></div>"##,
    );
    let person = items(&r["entities"])
        .iter()
        .find(|e| e["types"] == json!(["Person"]))
        .expect("Person entity");
    assert!(is_empty(&person["errors"]));
    assert!(is_empty(&person["warnings"]));
    assert_eq!(r["markup_coverage"][1]["status"], "checked");
}

// Public half only: the copy-graph node asserts read the reference `_extract_rdfa` (see the report).
#[test]
fn test_rdfa_property_copy_is_iterative_bounded_and_removes_patterns() {
    let markup = RDFA_COPY_GRAPH;
    let r = check_in(markup, "https://example.test/article/");
    assert_eq!(r["markup_coverage"][1]["status"], "checked");
}

#[test]
fn test_inert_templates_and_text_do_not_generate_markup_entities() {
    let r = check(
        r##"
        <template><div itemscope itemtype="https://schema.org/Product"></div>
          <div vocab="https://schema.org/" typeof="Product"></div></template>
        <p>itemscope vocab="https://schema.org/" typeof="Product"</p>"##,
    );
    assert!(is_empty(&r["entities"]));
    assert_eq!(
        r["other_markup"],
        json!({"microdata": false, "rdfa": false})
    );
}

#[test]
fn test_mixed_formats_keep_independent_paths_and_validation_results() {
    let r = check(
        r##"
        <script type="application/ld+json">{"@type":"Article","headline":"A"}</script>
        <div itemscope itemtype="https://schema.org/Product"><span itemprop="bogus">X</span></div>
        <div vocab="https://schema.org/" typeof="Event"><span property="name">E</span></div>"##,
    );
    let entities = items(&r["entities"]);
    let formats: Vec<&Value> = entities.iter().map(|e| &e["format"]).collect();
    assert_eq!(
        formats,
        [&json!("jsonld"), &json!("microdata"), &json!("rdfa")]
    );
    let mut paths: Vec<&Value> = entities.iter().map(|e| &e["path"]).collect();
    paths.sort_by_key(|p| p.to_string());
    paths.dedup();
    assert_eq!(paths.len(), 3);
    assert_eq!(r["blocks_found"], 1);
    assert_eq!(r["blocks"], 1);
    assert!(is_empty(&entities[0]["errors"]));
    assert!(!is_empty(&entities[1]["errors"]));
    let event = items(&r["rich_results"])
        .iter()
        .find(|x| x["type"] == "Event")
        .expect("Event rich result");
    assert_eq!(event["missing_required"], json!(["startDate", "location"]));
}

#[test]
fn test_cyclic_microdata_itemref_does_not_crash_or_disappear() {
    let r = check(
        r##"
        <div id="a" itemscope itemtype="https://schema.org/Person" itemprop="knows" itemref="b"></div>
        <div id="b" itemscope itemtype="https://schema.org/Person" itemprop="knows" itemref="a"></div>"##,
    );
    assert!(!is_empty(&r["entities"]));
    assert!(items(&r["entities"])
        .iter()
        .any(|node| items(&node["errors"])
            .iter()
            .any(|e| contains_str(e, "Cyclic"))));
}

#[test]
fn test_reused_rdfa_subject_does_not_create_recursive_python_objects() {
    let r = check(
        r##"
        <div vocab="https://schema.org/" typeof="Person" about="#person">
          <span property="name">Ada</span>
          <div property="knows" typeof="Person" about="#person"></div>
        </div>"##,
    );
    assert_eq!(r["entities"][0]["types"], json!(["Person"]));
    assert_eq!(r["ok"], true);
    assert_eq!(r["graph"]["nodes"], 1);
    assert_eq!(r["graph"]["linked_by_id"], 0);
}

#[test]
fn test_rdfa_resource_reference_is_connected_to_its_typed_subject() {
    let r = check(
        r##"
        <div vocab="https://schema.org/">
          <div typeof="Article" about="#article"><a property="author" resource="#ada">Ada</a></div>
          <div typeof="Person" about="#ada"><span property="name">Ada</span></div>
        </div>"##,
    );
    assert!(items(&r["entities"])
        .iter()
        .all(|e| is_empty(&e["errors"]) && is_empty(&e["warnings"])));
    assert_eq!(r["graph"]["linked_by_id"], 1);
    assert_eq!(r["graph"]["islands"], json!([]));
}

// Public half only: the preserved-node asserts read the reference `_extract_rdfa` (see the report).
#[test]
fn test_rdfa_malformed_resource_is_preserved_and_marks_coverage_partial() {
    let base = "https://example.test/article/";
    let html = r##"
    <div vocab="https://schema.org/" typeof="Person" about="#ada">
      <a property="knows" href="http://[">Broken reference</a>
    </div>"##;
    let r = check_in(html, base);
    assert_eq!(r["markup_coverage"][1]["status"], "partial");
    assert_eq!(
        r["markup_coverage"][1]["reasons"],
        json!(["RDFa IRI is malformed and could not be resolved"])
    );
    assert!(items(&r["entities"]).iter().any(|e| items(&e["errors"])
        .iter()
        .any(|x| contains_str(x, "http://["))));
}

#[test]
fn test_literal_number_and_date_ranges_are_validated() {
    for format in ["microdata", "rdfa"] {
        let (scope, prop) = if format == "microdata" {
            (
                r##"itemscope itemtype="https://schema.org/Article""##,
                "itemprop",
            )
        } else {
            (
                r##"vocab="https://schema.org/" typeof="Article""##,
                "property",
            )
        };
        let r = check(&format!(
            r##"
        <div {scope}><meta {prop}="wordCount" content="three hundred">
          <meta {prop}="datePublished" content="2026-99-99"></div>"##
        ));
        let warnings = items(&r["entities"][0]["warnings"]);
        assert!(
            warnings
                .iter()
                .any(|w| contains_str(w, "wordCount") && contains_str(w, "expected ranges")),
            "{format}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| contains_str(w, "datePublished") && contains_str(w, "expected ranges")),
            "{format}"
        );
    }
}

#[test]
fn test_blank_required_property_is_missing_for_rich_result() {
    let r = check(
        r##"
        <div itemscope itemtype="https://schema.org/Product">
          <meta itemprop="name" content="  ">
          <div itemprop="offers" itemscope itemtype="https://schema.org/Offer"></div>
        </div>"##,
    );
    assert_eq!(items(&r["rich_results"])[0]["eligible"], false);
    assert_eq!(
        items(&r["rich_results"])[0]["missing_required"],
        json!(["name"])
    );
}

#[test]
fn test_document_base_resolves_microdata_identity() {
    let r = check_url(
        "https://example.test/page",
        r##"
        <base href="/news/">
        <div itemscope itemtype="https://schema.org/Article" itemid="#article"></div>"##,
    );
    assert_eq!(r["entities"][0]["id"], "https://example.test/news/#article");
}

#[test]
fn test_deep_rdfa_reports_validation_limit() {
    let mut html = String::from(r##"<div vocab="https://schema.org/" typeof="Person">"##);
    html.push_str(&r##"<div property="knows" typeof="Person">"##.repeat(70));
    html.push_str(&"</div>".repeat(71));
    let r = check(&html);
    assert!(items(&r["entities"])
        .iter()
        .any(|node| items(&node["errors"])
            .iter()
            .any(|e| contains_str(e, "depth limit"))));
}

#[test]
fn test_untyped_microdata_names_the_html_attribute_to_fix() {
    let html = "<div itemscope><span itemprop=\"name\">X</span></div>";
    let report = check(html);
    let first_error = report["entities"][0]["errors"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(first_error.starts_with("Missing itemtype;"));
}

#[test]
fn test_foreign_redeclaration_does_not_erase_explicit_schema_type() {
    let r = check(
        r##"
        <div vocab="https://schema.org/" typeof="Product" about="#item"><span property="name">X</span></div>
        <div vocab="https://example.org/" typeof="Product" about="#item"></div>"##,
    );
    let types: Vec<&Value> = items(&r["rich_results"])
        .iter()
        .map(|x| &x["type"])
        .collect();
    assert_eq!(types, [&json!("Product")]);
    let product = items(&r["entities"])
        .iter()
        .find(|e| e["types"] == json!(["Product"]))
        .expect("Product entity");
    assert!(items(&product["warnings"])
        .iter()
        .any(|w| contains_str(w, "Foreign vocabulary https://example.org/")));
}

#[test]
fn test_relative_identity_links_jsonld_and_html_without_false_dangling_reference() {
    let cases = [
        ("microdata", "", "https://example.test/page"),
        (
            "microdata",
            r##"<base href="news/">"##,
            "https://example.test/news/",
        ),
        ("rdfa", "", "https://example.test/page"),
        (
            "rdfa",
            r##"<base href="news/">"##,
            "https://example.test/news/",
        ),
    ];
    for (format, base_tag, expected_base) in cases {
        let declaration = if format == "microdata" {
            r##"itemscope itemtype="https://schema.org/Person" itemid="#ada""##
        } else {
            r##"vocab="https://schema.org/" typeof="Person" about="#ada""##
        };
        let prop = if format == "microdata" {
            "itemprop"
        } else {
            "property"
        };
        let mut html = String::from(base_tag);
        html.push_str(
            r##"
        <script type="application/ld+json">
          {"@context":"https://schema.org","@type":"Article","@id":"#article",
           "author":{"@id":"#ada"}}
        </script>"##,
        );
        html.push_str(&format!(
            r##"<div {declaration}><span {prop}="name">Ada</span></div>"##
        ));
        let r = check_url("https://example.test/page", &html);
        let label = format!("{format} {base_tag}");
        let ids: std::collections::BTreeSet<String> = items(&r["entities"])
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect();
        let expected: std::collections::BTreeSet<String> = [
            format!("{expected_base}#article"),
            format!("{expected_base}#ada"),
        ]
        .into_iter()
        .collect();
        assert_eq!(ids, expected, "{label}");
        assert!(
            items(&r["entities"]).iter().all(|e| is_empty(&e["errors"])),
            "{label}"
        );
        assert_eq!(r["graph"]["islands"], json!([]), "{label}");
        assert_eq!(r["graph"]["is_graph"], true, "{label}");
        assert_eq!(r["graph"]["linked_by_id"], 1, "{label}");
    }
}

#[test]
fn test_resolved_document_base_is_not_applied_twice() {
    let html = r##"<base href="news/">
        <script type="application/ld+json">{"@type":"Article","@id":"#article","author":{"@id":"#ada"}}</script>
        <div itemscope itemtype="https://schema.org/Person" itemid="#ada"></div>"##;
    let r = check_in(html, "https://example.test/news/");
    let ids: std::collections::BTreeSet<String> = items(&r["entities"])
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    let expected: std::collections::BTreeSet<String> = [
        "https://example.test/news/#article".to_owned(),
        "https://example.test/news/#ada".to_owned(),
    ]
    .into_iter()
    .collect();
    assert_eq!(ids, expected);
    assert!(items(&r["entities"]).iter().all(|e| is_empty(&e["errors"])));
}

#[test]
fn test_identity_normalization_copies_source_and_preserves_foreign_context_and_blank_ids() {
    let payload = json!({
        "@context": "https://schema.org",
        "@type": "Article",
        "@id": "#article",
        "author": {"@id": "#ada"},
        "publisher": {
            "@context": "https://example.org/",
            "@type": "Organization",
            "@id": "#foreign",
        },
        "about": {"@id": "_:blank"},
    });
    let copied = normalize_jsonld_ids(&payload, "https://example.test/page").unwrap();
    assert_eq!(payload["@id"], "#article");
    assert_eq!(payload["author"]["@id"], "#ada");
    assert_eq!(copied["@id"], "https://example.test/page#article");
    assert_eq!(copied["author"]["@id"], "https://example.test/page#ada");
    assert_eq!(copied["publisher"]["@id"], "#foreign");
    assert_eq!(copied["about"]["@id"], "_:blank");
    assert_eq!(
        normalize_jsonld_ids(&payload["publisher"], "https://example.test/").unwrap()["@id"],
        "#foreign"
    );
}

#[test]
fn test_jsonld_local_base_and_foreign_vocab_context_are_respected() {
    let mut payload = json!({
        "@context": {"@vocab": "https://schema.org/", "@base": "people/"},
        "@type": "Person",
        "@id": "ada",
    });
    assert_eq!(
        normalize_jsonld_ids(&payload, "https://example.test/").unwrap()["@id"],
        "https://example.test/people/ada"
    );
    payload["@context"]["@base"] = Value::Null;
    assert_eq!(
        normalize_jsonld_ids(&payload, "https://example.test/").unwrap()["@id"],
        "ada"
    );
    payload["@context"]["@vocab"] = json!("https://example.org/");
    assert_eq!(
        normalize_jsonld_ids(&payload, "https://example.test/").unwrap()["@id"],
        "ada"
    );
}
