use super::*;
use serde_json::json;

const UUID: &str = "9097a379-f69e-4c0d-b790-3ad5156919f3";

fn obj(value: Value) -> Obj {
    value.as_object().cloned().unwrap_or_default()
}

fn rows_by_check(document: Value) -> HashMap<String, Obj> {
    capability_rows(&obj(document))
        .unwrap()
        .into_iter()
        .map(|row| (row["check"].as_str().unwrap().to_string(), row))
        .collect()
}

/// An in-memory scan with just the tables the contract reads.
fn scan(source_kind: &str) -> Connection {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE scan(singleton INTEGER, scan_uuid TEXT, source_kind TEXT);
         CREATE TABLE urls(url_id INTEGER PRIMARY KEY, url TEXT);
         CREATE TABLE documents(document_id INTEGER PRIMARY KEY, source_response_id INTEGER);
         CREATE TABLE responses(response_id INTEGER PRIMARY KEY);
         CREATE TABLE pages(url_id INTEGER, document_id INTEGER, representation TEXT);
         CREATE TABLE context_items(kind TEXT, item_key TEXT, payload_json TEXT);
         CREATE TABLE resource_graph_fetches(resolved_url TEXT, status_code INTEGER, bytes_received INTEGER);
         CREATE TABLE resource_graph_occurrences(occurrence_id INTEGER, resolved_url TEXT, page_url_id INTEGER, representation TEXT, source_document_id INTEGER);
         INSERT INTO urls VALUES(7,'https://e.test/a'),(8,'https://e.test/queued');
         INSERT INTO responses VALUES(5);
         INSERT INTO documents VALUES(3,5);
         INSERT INTO pages VALUES(7,3,'static');",
    )
    .unwrap();
    con.execute(
        "INSERT INTO scan VALUES(1,?1,?2)",
        params![UUID, source_kind],
    )
    .unwrap();
    con
}

#[test]
fn capability_prerequisites_follow_declared_sources_not_check_name_heuristics() {
    let rows = rows_by_check(json!({"summary": {}, "run": {}}));
    let response = &rows["BROKEN_PAGE_4XX"]["prerequisites"];
    assert_eq!(
        response["required_evidence"],
        json!({
            "state": "derived",
            "items": [{"kind": "screaming_frog_export", "source": "SF:Response Codes:4xx"}],
            "reason": "",
        })
    );
    assert_eq!(response["representation"]["state"], "unknown");
    assert!(response["issue_map_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference["name"] == "Internal Client Error (4XX)"));

    let sitemap = &rows["SITEMAP_DESYNC"]["prerequisites"];
    assert_eq!(
        sitemap["population"],
        json!({"state": "derived", "value": "sitemap declarations", "reason": ""})
    );
    assert_eq!(sitemap["representation"]["state"], "not_applicable");

    let derived = &rows["TITLE_MISSING"]["prerequisites"];
    assert_eq!(derived["source_tag"], "SF-derived");
    for key in ["required_evidence", "population", "representation"] {
        assert_eq!(derived[key]["state"], "unknown", "{key}");
    }
}

#[test]
fn prerequisites_cover_every_source_family() {
    for (source, evidence_kind) in [
        ("inlinks:All", "screaming_frog_inlinks_export"),
        ("crawl:html", "native_crawl_evidence"),
        ("provider:gsc", "retained_project_provider_history"),
        ("sitemap", "sitemap_observations"),
    ] {
        let result = prerequisites("TITLE_MISSING", source);
        assert_eq!(
            result["required_evidence"]["items"][0]["kind"],
            evidence_kind
        );
    }
    let provider = prerequisites("TITLE_MISSING", "provider:gsc");
    assert_eq!(provider["representation"]["state"], "not_applicable");
}

// Expected values fixed by the audit format.
#[test]
fn stable_evidence_id_matches_the_oracle() {
    assert_eq!(
        stable_evidence_id(UUID, "audit", "issue:ISSUE-000001").unwrap(),
        format!("evidence:v1:{UUID}:audit:e87ab075e302991707d210fe")
    );
    // Any UUID spelling, and ensure_ascii escaping of non-ASCII, quotes, backslash and DEL.
    assert_eq!(
        stable_evidence_id(
            &UUID.to_uppercase().replace('-', ""),
            "pages",
            "url:/ü/\u{1F600}\"\\\u{7f}"
        )
        .unwrap(),
        format!("evidence:v1:{UUID}:pages:1e858fbff8a65330aaf08b67")
    );
}

#[test]
fn stable_evidence_id_rejects_malformed_identities() {
    let long = "x".repeat(257);
    for (uuid, table, observation) in [
        ("not-a-uuid", "audit", "a"),
        (UUID, "Audit", "a"),
        (UUID, "1audit", "a"),
        (UUID, "audit", ""),
        (UUID, "audit", long.as_str()),
    ] {
        assert!(
            stable_evidence_id(uuid, table, observation).is_err(),
            "{uuid} {table}"
        );
    }
    assert!(stable_evidence_id(UUID, "audit", &"x".repeat(256)).is_ok());
}

#[test]
fn scan_uuid_accepts_python_spellings_only() {
    assert_eq!(
        scan_uuid(&json!(format!("{{{UUID}}}"))).as_deref(),
        Some(UUID)
    );
    assert_eq!(
        scan_uuid(&json!(format!("urn:uuid:{UUID}"))).as_deref(),
        Some(UUID)
    );
    assert_eq!(scan_uuid(&json!("zz")), None);
    assert_eq!(scan_uuid(&json!(5)), None);
}

#[test]
fn items_keep_only_registered_checks_with_reasons() {
    let value = json!([
        {"id": "TITLE_MISSING", "reason": "off"},
        {"id": "NOT_A_CHECK", "reason": "off"},
        {"id": "TITLE_MISSING"},
        "junk"
    ]);
    assert_eq!(
        items(&value).unwrap(),
        vec![("TITLE_MISSING".to_string(), "off".to_string())]
    );
    assert!(items(&json!({"id": "x"})).unwrap().is_empty());
}

#[test]
fn population_reads_only_well_typed_fields() {
    let document = obj(json!({
        "run": {"crawl_valid": false, "crawl_partial": 1},
        "summary": {"health_score_scope": "scope", "totals": {
            "urls_crawled": 12, "pages_by_representation": {"static": 12}}},
    }));
    assert_eq!(
        Value::Object(population(&document)),
        json!({"urls_crawled": 12, "crawl_valid": false, "crawl_partial": true,
               "scope_reason": "scope", "representations": {"static": 12}})
    );
    let blank = population(&obj(json!({"summary": {"totals": {"urls_crawled": -1}}})));
    assert_eq!(blank["urls_crawled"], Value::Null);
    assert_eq!(blank["crawl_valid"], true);
}

#[test]
fn capability_rows_separate_measured_skipped_disabled_and_unaccounted() {
    let rows = rows_by_check(json!({
        "run": {
            "crawl_partial": true,
            "checks_skipped": [{"id": "TITLE_MISSING", "reason": "no titles retained"}],
            "checks_disabled": [{"id": "H1_MISSING", "reason": "turned off"}],
        },
        "summary": {"check_coverage": {
            "checks_disabled_ids": ["H1_MISSING", "H1_MULTIPLE"],
            "checks_silent_ids": ["TITLE_DUPLICATE", "NOT_A_CHECK"],
        }},
        "issues": [{"check": "BROKEN_PAGE_4XX"}, {"check": "BOGUS"}],
    }));
    let state = |id: &str| {
        let row = &rows[id];
        (
            row["state"].as_str().unwrap(),
            row["capability"].as_str().unwrap(),
            row["reason"].as_str().unwrap(),
        )
    };
    assert_eq!(
        state("BROKEN_PAGE_4XX"),
        ("measured", "finding_recorded", "")
    );
    assert_eq!(
        state("TITLE_MISSING"),
        ("unmeasured", "skipped", "no titles retained")
    );
    assert_eq!(
        state("H1_MISSING"),
        ("unmeasured", "disabled", "turned off")
    );
    assert_eq!(
        state("H1_MULTIPLE"),
        (
            "unmeasured",
            "disabled",
            "disabled by the saved audit configuration"
        )
    );
    assert_eq!(
        state("TITLE_DUPLICATE"),
        (
            "partial",
            "ran_without_finding",
            "crawl was partial; this clean result covers only the saved scope"
        )
    );
    assert_eq!(
        state("TITLE_TOO_LONG"),
        (
            "unmeasured",
            "unaccounted",
            "the saved audit does not account for this check"
        )
    );
    assert!(!rows.contains_key("BOGUS"));
}

#[test]
fn finding_projection_names_the_audit_issue() {
    let issue = obj(json!({"id": "ISSUE-000001"}));
    let found = finding_projection(&issue, Some(UUID)).unwrap();
    assert_eq!(found["observation_id"], "issue:ISSUE-000001");
    assert_eq!(
        found["id"],
        format!("evidence:v1:{UUID}:audit:e87ab075e302991707d210fe")
    );
    assert_eq!(
        finding_projection(&issue, None).unwrap()["state"],
        "unavailable"
    );
    let bad = finding_projection(&obj(json!({"id": "ISSUE-12"})), Some(UUID)).unwrap();
    assert_eq!(
        bad["reason"],
        "saved audit finding has no stable ISSUE identifier"
    );
}

#[test]
fn saved_scan_names_every_binding_failure() {
    let con = scan("native");
    assert_eq!(
        saved_scan(None, Some(UUID)),
        Err("underlying saved observations were not supplied")
    );
    assert_eq!(
        saved_scan(Some(&con), None),
        Err("saved scan has no valid UUID binding")
    );
    assert_eq!(saved_scan(Some(&con), Some(UUID)), Ok("native".to_string()));
    assert_eq!(
        saved_scan(Some(&con), Some("00000000-0000-0000-0000-000000000000")),
        Err("saved scan identity does not match the audit binding")
    );
    assert_eq!(
        saved_scan(Some(&scan("mystery")), Some(UUID)),
        Err("saved scan source kind is unsupported")
    );
    let empty = Connection::open_in_memory().unwrap();
    assert_eq!(
        saved_scan(Some(&empty), Some(UUID)),
        Err("saved scan cannot resolve retained observations")
    );
    con.execute("DELETE FROM scan", []).unwrap();
    assert_eq!(
        saved_scan(Some(&con), Some(UUID)),
        Err("saved scan header is unavailable")
    );
}

#[test]
fn stored_page_observations_bind_page_document_and_response() {
    let con = scan("native");
    let found =
        stored_page_observations(&con, UUID, "https://e.test/a", "target", "native").unwrap();
    let tables: Vec<_> = found
        .iter()
        .map(|row| row["source_table"].as_str().unwrap())
        .collect();
    assert_eq!(tables, ["pages", "urls", "documents", "responses"]);
    assert!(found.iter().all(|row| row["state"] == "measured"));
    assert_eq!(found[0]["representation"], "static");
    assert_eq!(found[1]["representation"], "");
    let imported =
        stored_page_observations(&con, UUID, "https://e.test/a", "source", "legacy_import")
            .unwrap();
    assert_eq!(imported[0]["state"], "imported_projection");
    // A queued URL has no page row, so it is not an observation.
    assert!(
        stored_page_observations(&con, UUID, "https://e.test/queued", "target", "native")
            .unwrap()
            .is_empty()
    );
    assert!(
        stored_page_observations(&con, UUID, "https://e.test/none", "target", "native")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn language_observations_reference_declarations_and_empty_states() {
    let con = scan("native");
    let key = "page:7:document:3:representation:static";
    let payload = json!({"declarations": [{"ordinal": 0}, {"ordinal": "x"}, {"ordinal": 2}]});
    con.execute(
        "INSERT INTO context_items VALUES('language_evidence', ?1, ?2)",
        params![key, payload.to_string()],
    )
    .unwrap();
    let target = obj(json!({"source_table": "documents", "role": "target",
        "observation_id": "document_id:3", "representation": "static"}));
    let found = language_observations(&con, UUID, std::slice::from_ref(&target), "native").unwrap();
    let ids: Vec<_> = found
        .iter()
        .map(|row| row["observation_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        [
            format!("language_evidence:{key}:ordinal:0"),
            format!("language_evidence:{key}:ordinal:2")
        ]
    );
    assert!(
        language_observations(&con, UUID, std::slice::from_ref(&target), "legacy_import")
            .unwrap()
            .is_empty()
    );

    con.execute(
        "UPDATE context_items SET payload_json=?1",
        params![json!({"state": "absent", "declarations": []}).to_string()],
    )
    .unwrap();
    let found = language_observations(&con, UUID, &[target], "native").unwrap();
    assert_eq!(
        found[0]["observation_id"],
        format!("language_evidence:{key}:state:absent")
    );
}

#[test]
fn image_resources_reference_fetches_and_declarations() {
    let con = scan("native");
    con.execute_batch(
        "INSERT INTO resource_graph_fetches VALUES('https://e.test/i.png', 200, 10);
         INSERT INTO resource_graph_occurrences VALUES(4,'https://e.test/i.png',7,'static',3);",
    )
    .unwrap();
    let issue = obj(
        json!({"check": "IMG_OVER_100KB", "target_url": "https://e.test/i.png",
        "details": {"source_page": "https://e.test/a", "document_id": 3}}),
    );
    let found = image_resource_observations(&con, &issue, UUID).unwrap();
    let roles: Vec<_> = found
        .iter()
        .map(|row| row["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["target_resource", "source_declaration"]);
    assert!(found[0]["observation_id"]
        .as_str()
        .unwrap()
        .starts_with("url_sha256:"));
    assert_eq!(found[1]["observation_id"], "occurrence_id:4");
    let other = obj(json!({"check": "TITLE_MISSING", "target_url": "https://e.test/i.png"}));
    assert!(image_resource_observations(&con, &other, UUID)
        .unwrap()
        .is_empty());
    let wrong_document = obj(
        json!({"check": "IMG_X", "target_url": "https://e.test/i.png",
        "details": {"source_page": "https://e.test/a", "document_id": 99}}),
    );
    assert_eq!(
        image_resource_observations(&con, &wrong_document, UUID)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn issue_reference_puts_observations_first_and_explains_absence() {
    let con = scan("native");
    let issue = obj(
        json!({"id": "ISSUE-000002", "check": "TITLE_MISSING", "target_url": "https://e.test/a"}),
    );
    let reference = issue_reference(&issue, Some(&con), Some(UUID)).unwrap();
    assert_eq!(reference["source_table"], "pages");
    assert_eq!(reference["observations"].as_array().unwrap().len(), 4);
    assert_eq!(reference["finding"]["role"], "finding_projection");

    let missing = obj(
        json!({"id": "ISSUE-000003", "check": "TITLE_MISSING", "target_url": "https://e.test/nope"}),
    );
    let none = issue_reference(&missing, Some(&con), Some(UUID)).unwrap();
    assert_eq!(none["state"], "unavailable");
    assert_eq!(
        none["reason"],
        "no retained page/document/response observation matches this finding"
    );

    let legacy = scan("legacy_import");
    let reason = issue_reference(&missing, Some(&legacy), Some(UUID)).unwrap();
    assert_eq!(
        reason["reason"],
        "legacy import has no captured page/document/response observations"
    );
    let unbound = issue_reference(&issue, None, None).unwrap();
    assert_eq!(
        unbound["reason"],
        "underlying saved observations were not supplied"
    );
}

#[test]
fn attach_contract_binds_identity_and_keeps_the_schema() {
    let document = obj(
        json!({"schema_version": "2.0", "run": {"scan_uuid": UUID}, "issues": [
        {"id": "ISSUE-000001", "check": "TITLE_MISSING", "evidence": {"kept": 1}}, "junk"]}),
    );
    let attached = attach_contract(&document, None, None).unwrap();
    assert_eq!(attached["schema_version"], "2.0");
    let contract = &attached["summary"]["evidence_contract"];
    assert_eq!(contract["scan_identity_state"], "measured");
    assert_eq!(contract["scan_uuid"], UUID);
    assert_eq!(
        contract["capability_rows"].as_array().unwrap().len(),
        registry().unwrap().len()
    );
    assert_eq!(attached["issues"][0]["evidence"]["kept"], 1);
    assert_eq!(
        attached["issues"][0]["evidence"]["contract"]["state"],
        "unavailable"
    );
    assert_eq!(attached["issues"][1], "junk");
    // The input is not modified.
    assert!(document.get("summary").is_none());

    let anonymous = attach_contract(&obj(json!({"schema_version": "2.0"})), None, None).unwrap();
    let identity = &anonymous["summary"]["evidence_contract"];
    assert_eq!(identity["scan_identity_state"], "unavailable");
    assert_eq!(
        identity["scan_identity_reason"],
        "no retained scan UUID in this audit document"
    );
    assert!(attach_contract(&obj(json!({"schema_version": "3.0"})), None, None).is_err());
}

#[test]
fn attach_saved_corpus_checks_the_derivation_version() {
    let con = scan("native");
    let document = obj(json!({"schema_version": "2.0"}));
    let wrong = obj(json!({"schema_version": "saved_corpus.v1"}));
    let sources = corpus_derivations::DefaultSources;
    assert!(attach_saved_corpus(&document, &con, 0.92, Some(&wrong), &sources).is_err());
    let right = obj(json!({"schema_version": SAVED_CORPUS_VERSION}));
    let attached = attach_saved_corpus(&document, &con, 0.92, Some(&right), &sources).unwrap();
    assert_eq!(
        attached["summary"]["saved_corpus_derivations"],
        Value::Object(right)
    );
    assert!(attach_saved_corpus(&obj(json!({})), &con, 0.92, None, &sources).is_err());
}

fn comparable(partial: Option<bool>) -> Obj {
    let mut run = json!({});
    if let Some(flag) = partial {
        run["corpus_partial"] = json!(flag);
    }
    obj(json!({"run": run, "summary": {
        "saved_corpus_derivations": {"schema_version": "saved_corpus.v1"},
        "evidence_contract": {"scan_identity_state": "measured"}}}))
}

fn basis(before: &Obj, after: &Obj, name: &str) -> Obj {
    comparison_compatibility(before, after)
        .into_iter()
        .find(|row| row["basis"] == name)
        .unwrap()
}

#[test]
fn corpus_completeness_unknown_is_not_equal_to_measured_complete() {
    for (partial, expected, warning) in [
        (None, "unknown", Some("unavailable")),
        (Some(false), "compatible", None),
        (Some(true), "compatible", Some("partial")),
    ] {
        let side = comparable(partial);
        assert_eq!(basis(&side, &side, "corpus")["state"], expected);
        let corpus: Vec<_> = comparison_warnings(&side, &side)
            .into_iter()
            .filter(|text| text.contains("corpus"))
            .collect();
        match warning {
            None => assert!(corpus.is_empty()),
            Some(word) => assert!(corpus.iter().any(|text| text.contains(word)), "{corpus:?}"),
        }
    }
}

#[test]
fn missing_corpus_flag_on_one_side_stays_unknown() {
    let (before, after) = (comparable(None), comparable(Some(false)));
    let row = basis(&before, &after, "corpus");
    assert_eq!(row["state"], "unknown");
    assert_eq!(row["before"]["corpus_partial"], Value::Null);
    assert_eq!(row["after"]["corpus_partial"], false);
}

#[test]
fn partial_unknown_and_incompatible_bases_stay_visible() {
    let before = obj(json!({"run": {"crawl_valid": true, "crawl_partial": false,
        "crawl_config": {"robots.enabled": true}}}));
    let after = obj(json!({"run": {"crawl_valid": false, "crawl_partial": true,
        "crawl_config": {"robots.enabled": false}}}));
    let rows = comparison_compatibility(&before, &after);
    assert_eq!(rows.len(), 5);
    assert!(rows.iter().any(|row| row["state"] == "unknown"));
    assert_eq!(basis(&before, &after, "scope")["state"], "incompatible");
    assert_eq!(
        basis(&before, &after, "configuration")["state"],
        "incompatible"
    );
    let warnings = comparison_warnings(&before, &after);
    assert!(warnings.contains(&"comparison scope basis differs between saved audits".to_string()));
    assert!(warnings.contains(
        &"comparison provider basis is unavailable; comparability is unknown".to_string()
    ));
}

#[test]
fn provider_and_representation_bases_compare_by_value() {
    let run = |mode: &str| {
        json!({"run": {"input_mode": mode, "collector": "c"},
        "summary": {"totals": {"pages_by_representation": {"static": 1}}}})
    };
    let (a, b) = (obj(run("crawl")), obj(run("import")));
    assert_eq!(basis(&a, &a, "provider")["state"], "compatible");
    assert_eq!(basis(&a, &b, "provider")["state"], "incompatible");
    assert_eq!(basis(&a, &b, "representation")["state"], "compatible");
}
