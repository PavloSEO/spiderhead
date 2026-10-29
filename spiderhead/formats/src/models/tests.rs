//! Tests of the normalized data model and its audit.v2 projection.

use super::*;
use serde_json::json;

/// A JSON object literal as an [`Object`].
fn object(value: Value) -> Object {
    match value {
        Value::Object(map) => map,
        _ => Object::new(),
    }
}

fn sitemap_result() -> AuditResult {
    let mut summary = Object::new();
    summary.insert(
        "sitemap".into(),
        json!({
            "urls_in_sitemap": 2,
            "linked_not_in_sitemap": ["https://example.test/a", "https://example.test/b"],
        }),
    );
    AuditResult {
        summary,
        ..AuditResult::default()
    }
}

#[test]
fn audit_v2_moves_sitemap_url_lists_out_of_the_header_without_losing_compatibility() {
    let result = sitemap_result();
    let parts = result.audit_v2_parts();
    let pointer = "/summary/sitemap/linked_not_in_sitemap";
    assert_eq!(
        parts.header["summary"]["sitemap"]["linked_not_in_sitemap"],
        json!([])
    );
    let (_, rows) = parts
        .collections
        .iter()
        .find(|(name, _)| name == pointer)
        .expect("sitemap collection present");
    let streamed: Vec<Value> = rows.iter().collect();
    assert_eq!(
        streamed,
        [
            json!("https://example.test/a"),
            json!("https://example.test/b")
        ]
    );
    assert_eq!(
        result.summary["sitemap"]["linked_not_in_sitemap"],
        json!(["https://example.test/a", "https://example.test/b"])
    );
    assert_eq!(
        result.to_json()["summary"]["sitemap"]["linked_not_in_sitemap"],
        json!(["https://example.test/a", "https://example.test/b"])
    );
}

// Streamed rows restored into the header reproduce the document.
#[test]
fn streamed_collections_restored_into_the_header_reproduce_the_document() {
    let result = AuditResult {
        run: object(json!({"input_mode": "crawl"})),
        summary: object(json!({"sitemap": {"linked_not_in_sitemap": ["https://example.test/a"]}})),
        issues: vec![Issue::new("TITLE_MISSING", "warning", "fixture", "x")],
        pages: vec![Page::new("https://example.test/")],
        ..AuditResult::default()
    };
    let expected = result.to_json();
    let parts = result.audit_v2_parts();
    let mut header = parts.header.clone();
    for (pointer, rows) in &parts.collections {
        let items: Vec<Value> = rows.iter().collect();
        audit_v2::set_collection(&mut header, pointer, items);
    }
    assert_eq!(Value::Object(header), Value::Object(expected));
}

// Checks collection counts only: one row per issue, page and group.
#[test]
fn each_collection_streams_one_row_per_item_and_can_be_iterated_twice() {
    let result = AuditResult {
        issues: vec![Issue::new("TITLE_MISSING", "warning", "fixture", "x")],
        pages: vec![Page::new("https://example.test/")],
        groups: vec![Group {
            group_id: "GRP-TITLE-0001".into(),
            check: "TITLE_MISSING".into(),
            value: Some(String::new()),
            urls: vec![],
            count: 0,
        }],
        ..AuditResult::default()
    };
    let parts = result.audit_v2_parts();
    let names: Vec<&str> = parts.collections.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["/issues", "/pages", "/groups"]);
    for (_, rows) in &parts.collections {
        assert_eq!(rows.iter().count(), 1);
        assert_eq!(rows.iter().count(), 1, "second pass sees the same rows");
    }
}

#[test]
fn header_keeps_the_oracle_key_order_and_empty_placeholders() {
    let result = AuditResult {
        suppressed_issues: vec![json!({"check": "X"})],
        ..AuditResult::default()
    };
    let parts = result.audit_v2_parts();
    let keys: Vec<&str> = parts.header.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "tool",
            "run",
            "summary",
            "issues",
            "pages",
            "groups",
            "suppressed_issues"
        ]
    );
    assert_eq!(parts.header["tool"]["name"], json!("Export Analyzer"));
    assert_eq!(parts.header["tool"]["generated_by"], json!("SEOHead"));
    assert_eq!(parts.header["suppressed_issues"], json!([]));
}

#[test]
fn suppressed_issues_collection_appears_only_when_non_empty() {
    let empty = AuditResult::default();
    let bare = empty.audit_v2_parts();
    assert!(bare.header.get("suppressed_issues").is_none());
    assert!(bare
        .collections
        .iter()
        .all(|(n, _)| n != "/suppressed_issues"));
    assert_eq!(empty.to_json().get("suppressed_issues"), None);
}

#[test]
fn run_gains_checks_skipped_and_disabled_lists() {
    let result = AuditResult {
        run: object(json!({"mode": "crawl"})),
        skipped: vec![SkippedCheck {
            id: "A".into(),
            reason: "no data".into(),
        }],
        disabled: vec![SkippedCheck {
            id: "B".into(),
            reason: "off".into(),
        }],
        ..AuditResult::default()
    };
    let doc = result.to_json();
    assert_eq!(
        doc["run"]["checks_skipped"],
        json!([{"id": "A", "reason": "no data"}])
    );
    assert_eq!(
        doc["run"]["checks_disabled"],
        json!([{"id": "B", "reason": "off"}])
    );
    assert_eq!(doc["run"]["mode"], json!("crawl"));
}

#[test]
fn group_urls_are_a_json_array_in_the_document() {
    let group = Group {
        group_id: "GROUP".into(),
        check: "TITLE_DUPLICATE".into(),
        value: Some("shared".into()),
        urls: vec!["a".into(), "b".into()],
        count: 2,
    };
    assert_eq!(
        Value::Object(group.to_json()),
        json!({"group_id": "GROUP", "check": "TITLE_DUPLICATE", "value": "shared", "urls": ["a", "b"], "count": 2})
    );
}

#[test]
fn set_collection_creates_missing_objects_and_unescapes_the_pointer() {
    let mut document = Object::new();
    audit_v2::set_collection(&mut document, "/summary/sitemap/a~1b~0c", vec![json!(1)]);
    assert_eq!(
        Value::Object(document),
        json!({"summary": {"sitemap": {"a/b~c": [1]}}})
    );
    assert_eq!(escape_pointer_token("a/b~c"), "a~1b~0c");
}

// Table-driven: content-type and status predicates.
#[test]
fn page_predicates_read_content_type_status_and_indexability_separately() {
    let cases = [
        // (content_type, status, indexability, is_html, is_2xx, is_indexable)
        (
            Some("text/html; charset=utf-8"),
            Some(200),
            Some("Indexable"),
            true,
            true,
            true,
        ),
        (
            Some("TEXT/HTML"),
            Some(301),
            Some(" indexable "),
            true,
            false,
            true,
        ),
        (
            Some("text/html"),
            Some(404),
            Some("Non-Indexable"),
            true,
            false,
            false,
        ),
        (Some("application/pdf"), Some(200), None, false, true, false),
        (None, None, None, false, false, false),
    ];
    for (content_type, status, indexability, html, two_xx, indexable) in cases {
        let page = Page {
            content_type: content_type.map(str::to_owned),
            status_code: status,
            indexability: indexability.map(str::to_owned),
            ..Page::new("https://example.test/")
        };
        assert_eq!(page.is_html(), html, "{content_type:?}");
        assert_eq!(page.is_2xx(), two_xx, "{status:?}");
        assert_eq!(page.is_indexable(), indexable, "{indexability:?}");
    }
}

#[test]
fn page_row_writes_suppressed_ids_only_when_present() {
    let mut page = Page::new("https://example.test/");
    page.metrics.insert("title".into(), json!(""));
    let row = page.to_json();
    assert!(row.get("suppressed_issue_ids").is_none());
    assert_eq!(row["metrics"], json!({"title": ""}));
    page.suppressed_issue_ids = vec!["X".into()];
    assert_eq!(page.to_json()["suppressed_issue_ids"], json!(["X"]));
}

#[test]
fn link_location_has_the_eight_location_fields_and_no_destination() {
    let link = Link {
        source_url: Some("https://example.test/a".into()),
        destination_url: Some("https://example.test/b".into()),
        anchor: Some("more".into()),
        follow: Some(true),
        ..Link::default()
    };
    let location = link.as_location();
    let keys: Vec<&str> = location.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "source_url",
            "anchor",
            "alt_text",
            "link_position",
            "link_path",
            "follow",
            "rel",
            "target"
        ]
    );
    assert_eq!(location["follow"], json!(true));
    assert_eq!(location["rel"], Value::Null);
}

#[test]
fn issue_row_leaves_out_empty_optional_collections() {
    let issue = Issue::new("TITLE_MISSING", "warning", "fixture", "x");
    let row = issue.to_json();
    for key in ["locations", "details", "group_id", "fix_hint", "evidence"] {
        assert!(row.get(key).is_none(), "{key}");
    }
    assert_eq!(row["occurrences_count"], json!(1));
    let mut grouped = issue.clone();
    grouped.group_id = Some("G".into());
    grouped.fix_hint = Some(String::new());
    let row = grouped.to_json();
    assert_eq!(row["group_id"], json!("G"));
    assert!(row.get("fix_hint").is_none(), "an empty hint is dropped");
}

#[test]
fn image_source_needs_an_img_check_and_a_non_empty_source_page() {
    let mut issue = Issue::new("IMG_OVER_KB", "notice", "fixture", "x");
    assert_eq!(issue.image_source(), None);
    issue
        .details
        .insert("source_page".into(), json!("https://example.test/p"));
    assert_eq!(issue.image_source(), Some("https://example.test/p"));
    issue.check = "TITLE_MISSING".into();
    assert_eq!(
        issue.image_source(),
        None,
        "only IMG_ checks are bound to a source page"
    );
    issue.check = "IMG_ALT_TOO_LONG".into();
    issue.details.insert("source_page".into(), json!(""));
    assert_eq!(issue.image_source(), None);
}

#[test]
fn dedupe_key_takes_scope_only_for_image_issues() {
    let mut image = Issue::new("IMG_OVER_KB", "notice", "fixture", "x");
    image.target_url = Some("https://example.test/i.png".into());
    image
        .details
        .insert("source_page".into(), json!("https://example.test/p"));
    image
        .details
        .insert("target_scope".into(), json!("image_element"));
    let key = image.dedupe_key();
    assert_eq!(key.image_source.as_deref(), Some("https://example.test/p"));
    assert_eq!(key.scope.as_deref(), Some("image_element"));

    let mut plain = Issue::new("BROKEN_INTERNAL_LINK", "critical", "inlinks", "x");
    plain
        .details
        .insert("target_scope".into(), json!("image_element"));
    let key = plain.dedupe_key();
    assert_eq!(key.image_source, None);
    assert_eq!(key.scope, None, "non-image issues ignore target_scope");
}

#[test]
fn merging_plain_issues_counts_distinct_sources() {
    let mut first = Issue::new("DESC_MISSING", "warning", "crawl", "x");
    first.locations = vec![object(json!({"source_url": "https://example.test/a"}))];
    let mut second = first.clone();
    second.locations = vec![
        object(json!({"source_url": "https://example.test/b"})),
        object(json!({"source_url": "https://example.test/a"})),
    ];
    second.occurrences_count = 1;
    first.merge(&second);
    assert_eq!(first.locations.len(), 3);
    assert_eq!(first.occurrences_count, 2, "two distinct source pages");
}

#[test]
fn merging_image_issues_caps_locations_and_counts_the_rest() {
    let location = |n: usize| object(json!({"source_url": format!("https://example.test/{n}")}));
    let mut first = Issue::new("IMG_OVER_KB", "notice", "crawl", "x");
    first
        .details
        .insert("source_page".into(), json!("https://example.test/p"));
    first.locations = (0..150).map(location).collect();
    let mut second = first.clone();
    second.locations = (150..250).map(location).collect();
    second.occurrences_count = 3;
    first.merge(&second);
    assert_eq!(first.locations.len(), 200);
    assert_eq!(first.details["locations_omitted"], json!(50));
    assert_eq!(first.occurrences_count, 4);
}

#[test]
fn merging_image_issues_ignores_invalid_omitted_counts() {
    let mut first = Issue::new("IMG_OVER_KB", "notice", "crawl", "x");
    first
        .details
        .insert("source_page".into(), json!("https://example.test/p"));
    first.details.insert("locations_omitted".into(), json!(5));
    let mut second = Issue::new("IMG_OVER_KB", "notice", "crawl", "x");
    second
        .details
        .insert("source_page".into(), json!("https://example.test/p"));
    second.details.insert("locations_omitted".into(), json!(-3));
    first.merge(&second);
    assert_eq!(
        first.details["locations_omitted"],
        json!(5),
        "the negative count counts as zero"
    );
}

#[test]
fn severities_are_listed_lowest_to_highest_in_the_oracle_order() {
    assert_eq!(SEVERITIES, ["critical", "warning", "notice", "opportunity"]);
}
