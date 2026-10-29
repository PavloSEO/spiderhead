use super::*;
use serde_json::json;

fn obj(value: Value) -> Obj {
    value.as_object().cloned().unwrap_or_default()
}

fn page(id: i64, url: &str) -> Obj {
    obj(
        json!({"url_id": id, "url": url, "document_id": id * 10, "status_code": 200,
        "redirect_url": null, "canonical": null, "meta_robots": null, "x_robots": null,
        "representation": "static", "hreflang_outside_head": 0,
        "body_state": "complete", "body_sha256": "ab"}),
    )
}

fn maps(pages: &[Obj]) -> PageMaps {
    let (mut by_id, mut by_norm) = (BTreeMap::new(), BTreeMap::<String, Vec<Obj>>::new());
    for item in pages {
        let id = item["url_id"].as_i64().unwrap();
        by_norm
            .entry(norm_url(item["url"].as_str()).unwrap())
            .or_default()
            .push(item.clone());
        by_id.insert(id, item.clone());
    }
    (by_id, by_norm)
}

fn evidence(page_id: i64, declarations: Value) -> Value {
    json!({"page_url_id": page_id, "source_document_id": page_id * 10, "representation": "static",
        "state": "declared", "declarations": declarations})
}

fn declaration(ordinal: i64, lang: &str, target: &str) -> Value {
    json!({"ordinal": ordinal, "lang": lang, "raw_href": target, "target": target, "state": "declared"})
}

#[test]
fn source_active_needs_a_complete_matching_document() {
    let source = page(1, "https://e.test/");
    let good = obj(evidence(1, json!([])));
    assert!(source_active(&good, Some(&source)));
    assert!(!source_active(&good, None));
    let mut stale = good.clone();
    stale.insert("source_document_id".into(), 99.into());
    assert!(!source_active(&stale, Some(&source)));
    let mut partial = source.clone();
    partial.insert("body_state".into(), "partial".into());
    assert!(!source_active(&good, Some(&partial)));
    let mut unknown = good.clone();
    unknown.insert("state".into(), "malformed".into());
    assert!(!source_active(&unknown, Some(&source)));
}

#[test]
fn relative_href_is_classified_like_urlsplit() {
    for (href, state) in [
        ("", "malformed"),
        ("https://e.test/a", "absolute"),
        ("//e.test/a", "absolute"),
        ("mailto:a@e.test", "absolute"),
        ("/fr/", "relative"),
        ("fr/page:1", "relative"),
        ("1x:a", "relative"),
    ] {
        assert_eq!(relative(href)["state"], state, "{href}");
    }
}

#[test]
fn placement_reports_only_stored_observations() {
    assert_eq!(placement(None)["state"], "unmeasured");
    let mut source = page(1, "https://e.test/");
    assert_eq!(
        placement(Some(&source))["reason"],
        "at least one declaration is in head"
    );
    source.insert("hreflang_outside_head".into(), 1.into());
    assert_eq!(placement(Some(&source))["all_outside_head"], true);
    source.insert("hreflang_outside_head".into(), Value::Null);
    assert_eq!(placement(Some(&source))["state"], "unmeasured");
}

#[test]
fn source_id_names_page_document_representation_and_ordinal() {
    let item = obj(json!({"source_page_url_id": 1, "source_document_id": 10,
        "representation": "static", "ordinal": 2}));
    assert_eq!(
        source_id(&item),
        "page:1:document:10:representation:static:ordinal:2"
    );
}

#[test]
fn indexability_stops_at_the_first_missing_fact() {
    let mut item = page(1, "https://e.test/");
    item.insert("status_code".into(), Value::Null);
    assert_eq!(
        indexability(&item).unwrap()["reason"],
        "captured page has no HTTP status"
    );
    item.insert("status_code".into(), 404.into());
    assert_eq!(indexability(&item).unwrap()["reason"], "HTTP 404");
    item.insert("status_code".into(), 200.into());
    item.insert("body_state".into(), "partial".into());
    assert_eq!(indexability(&item).unwrap()["state"], "unmeasured");
}

#[test]
fn indexability_reads_directives_and_canonical() {
    let mut item = page(1, "https://e.test/");
    assert_eq!(indexability(&item).unwrap()["state"], "indexable_candidate");
    item.insert("meta_robots".into(), "noindex".into());
    assert_eq!(indexability(&item).unwrap()["reason"], "noindex directive");
    item.insert("meta_robots".into(), Value::Null);
    item.insert("canonical".into(), "https://e.test/other".into());
    assert_eq!(
        indexability(&item).unwrap()["reason"],
        "canonicalized to another URL"
    );
}

#[test]
fn target_resolves_only_one_captured_page() {
    let pages = maps(&[
        page(1, "https://e.test/a"),
        page(2, "https://e.test/b/"),
        page(3, "https://e.test/b"),
    ]);
    assert_eq!(
        target("", &pages).unwrap()["reason"],
        "declaration has no resolved target"
    );
    assert_eq!(
        target("https://e.test/zzz", &pages).unwrap()["reason"],
        UNCAPTURED_TARGET_REASON
    );
    // `/b` and `/b/` share a normalized key: two variants are ambiguous.
    assert_eq!(
        target("https://e.test/b", &pages).unwrap()["reason"],
        "target has multiple captured URL variants in this scan"
    );
}

/// en <-> fr on two pages, plus an uncaptured third target from the English page.
fn bilingual() -> (PageMaps, Vec<Value>) {
    let pages = maps(&[page(1, "https://e.test/en/"), page(2, "https://e.test/fr/")]);
    let language = vec![
        evidence(
            1,
            json!([
                declaration(0, "en", "https://e.test/en/"),
                declaration(1, "fr", "https://e.test/fr/"),
                declaration(2, "de", "https://e.test/de/"),
                declaration(3, "xx", "https://e.test/en/"),
            ]),
        ),
        evidence(2, json!([declaration(0, "fr", "https://e.test/fr/")])),
    ];
    (pages, language)
}

fn state<'a>(row: &'a Obj, key: &str) -> &'a str {
    row[key]["state"].as_str().unwrap()
}

#[test]
fn language_derivations_validate_reciprocity_self_reference_and_fallback() {
    let (pages, language) = bilingual();
    let rows = language_derivations(&language, &pages, None).unwrap();
    assert_eq!(rows.len(), 5);
    // en -> fr: the French page does not point back to English.
    assert_eq!(state(&rows[1], "reciprocity"), "missing");
    assert_eq!(state(&rows[0], "self_reference"), "present");
    assert_eq!(state(&rows[0], "fallback"), "missing");
    // de was never captured.
    assert_eq!(state(&rows[2], "reciprocity"), "unmeasured");
    assert_eq!(
        rows[2]["target_observation"]["reason"],
        UNCAPTURED_TARGET_REASON
    );
    assert_eq!(rows[3]["code"]["state"], "invalid");
    assert_eq!(rows[0]["code"]["state"], "valid");
    assert_eq!(
        rows[0]["source_declaration_id"],
        "page:1:document:10:representation:static:ordinal:0"
    );
    assert_eq!(rows[1]["label_context"]["duplicate_pair_count"], 1);
    assert_eq!(rows[0]["href"]["state"], "absolute");
    // fr -> fr: the page's own return edge counts.
    assert_eq!(state(&rows[4], "reciprocity"), "present");
}

#[test]
fn a_mutual_pair_is_present_and_a_superseded_source_is_unmeasured() {
    let pages = maps(&[page(1, "https://e.test/en/"), page(2, "https://e.test/fr/")]);
    let language = vec![
        evidence(1, json!([declaration(0, "fr", "https://e.test/fr/")])),
        evidence(2, json!([declaration(0, "en", "https://e.test/en/")])),
    ];
    let rows = language_derivations(&language, &pages, None).unwrap();
    assert_eq!(state(&rows[0], "reciprocity"), "present");
    assert_eq!(state(&rows[1], "reciprocity"), "present");

    let mut stale = language.clone();
    stale[0]["source_document_id"] = 999.into();
    let rows = language_derivations(&stale, &pages, None).unwrap();
    assert_eq!(
        rows[0]["source_representation_state"],
        "superseded_or_unmeasured"
    );
    assert_eq!(
        rows[0]["reciprocity"]["reason"],
        "source representation is not the active captured page"
    );
    // The English page lost its evidence, so the French declaration cannot be checked.
    assert_eq!(state(&rows[1], "reciprocity"), "unmeasured");
}

#[test]
fn a_malformed_ordinal_is_not_a_declaration() {
    let pages = maps(&[page(1, "https://e.test/en/")]);
    let language = vec![evidence(
        1,
        json!([{"ordinal": -1, "lang": "en", "target": "https://e.test/en/", "state": "declared"}]),
    )];
    let rows = language_derivations(&language, &pages, None).unwrap();
    assert_eq!(rows[0]["declaration_state"], "malformed");
    assert_eq!(rows[0]["source_declaration_id"], "unavailable");
}

fn source_db(pages: &[Obj]) -> Connection {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE urls(url_id INTEGER PRIMARY KEY, url TEXT);
         CREATE TABLE documents(document_id INTEGER PRIMARY KEY, body_state TEXT, body_sha256 TEXT);
         CREATE TABLE pages(url_id INTEGER, document_id INTEGER, status_code INTEGER, redirect_url TEXT,
            canonical TEXT, meta_robots TEXT, x_robots TEXT, representation TEXT, hreflang_outside_head INTEGER);",
    )
    .unwrap();
    for item in pages {
        let (id, doc) = (
            item["url_id"].as_i64().unwrap(),
            item["document_id"].as_i64().unwrap(),
        );
        con.execute(
            "INSERT INTO urls VALUES(?1,?2)",
            params![id, item["url"].as_str()],
        )
        .unwrap();
        con.execute(
            "INSERT INTO documents VALUES(?1,'complete','ab')",
            params![doc],
        )
        .unwrap();
        con.execute(
            "INSERT INTO pages VALUES(?1,?2,200,NULL,NULL,NULL,NULL,'static',0)",
            params![id, doc],
        )
        .unwrap();
    }
    con
}

#[test]
fn page_rows_read_captured_pages_only() {
    let items = [page(1, "https://e.test/a"), page(2, "https://e.test/b")];
    let con = source_db(&items);
    con.execute("INSERT INTO urls VALUES(3,'https://e.test/queued')", [])
        .unwrap();
    let rows = page_rows(&con).unwrap();
    assert_eq!(rows, items.to_vec());
    let (by_id, by_norm) = page_index(&con).unwrap();
    assert_eq!(by_id.len(), 2);
    assert_eq!(by_norm.len(), 2);
}

#[test]
fn the_streaming_index_gives_the_same_derivations_and_removes_its_file() {
    let (pages, language) = bilingual();
    let items = [page(1, "https://e.test/en/"), page(2, "https://e.test/fr/")];
    let con = source_db(&items);
    let eager = language_derivations(&language, &pages, None).unwrap();
    let mut index = CorpusIndex::new(&con, &language).unwrap();
    let mut streamed = Vec::new();
    for item in &language {
        streamed.extend(
            language_derivations(std::slice::from_ref(item), &index, Some(&index)).unwrap(),
        );
    }
    assert_eq!(eager, streamed);
    assert_eq!(index.declaration_count(), 5);
    assert_eq!(index.evidence_count(1).unwrap(), 1);
    assert_eq!(index.evidence_count(9).unwrap(), 0);
    assert!(index
        .has_edge("static", "https://e.test/en", "https://e.test/fr")
        .unwrap());
    assert!(!index
        .has_edge("static", "https://e.test/fr", "https://e.test/en")
        .unwrap());
    assert_eq!(
        index
            .self_codes("https://e.test/en")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["en", "xx"]
    );
    assert_eq!(index.candidates("https://e.test/en").unwrap().len(), 1);
    assert!(index.get(7).unwrap().is_none());
    let path = index.path().to_path_buf();
    assert!(path.exists());
    index.close().unwrap();
    index.close().unwrap();
    assert!(!path.exists());
    assert!(index.get(1).is_err());
}
