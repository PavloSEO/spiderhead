//! Tests of the catalogue coverage map. Structural checks only: the check-id and command
//! existence checks need the check registry and the CLI surface;
//! the generated document check needs the coverage renderer.

use super::*;

/// The catalogue publishes 320 issues.
const TOTAL_PUBLISHED_ISSUES: usize = 320;

/// Expected counts (`coverage_counts()`).
const ORACLE_CHECK: usize = 216;
const ORACLE_TOOL: usize = 10;
const ORACLE_OUT_OF_SCOPE: usize = 94;

/// Category names in catalogue order.
const ORACLE_CATEGORY_ORDER: [&str; 24] = [
    "Response Codes",
    "Security",
    "URL",
    "Page Titles",
    "Meta Description",
    "H1",
    "H2",
    "Content",
    "Images",
    "Canonicals",
    "Pagination",
    "Directives",
    "Hreflang",
    "JavaScript",
    "Links",
    "Structured Data",
    "Sitemaps",
    "PageSpeed",
    "Mobile",
    "Accessibility",
    "Analytics",
    "Search Console",
    "Validation",
    "AMP",
];

fn category_entries(name: &str) -> Vec<Entry> {
    categories()
        .into_iter()
        .find(|category| category.name == name)
        .map(|category| category.entries)
        .unwrap_or_default()
}

#[test]
fn every_published_issue_is_accounted_for() {
    assert_eq!(entries().len(), TOTAL_PUBLISHED_ISSUES);
}

#[test]
fn every_entry_carries_exactly_one_known_status() {
    let names: Vec<&str> = Status::ALL.iter().map(|status| status.as_str()).collect();
    assert_eq!(names, ["check", "tool", "partial", "gap", "out_of_scope"]);
    for (category, entry) in entries() {
        assert!(
            Status::ALL.contains(&entry.status),
            "{category}/{}",
            entry.name
        );
    }
}

#[test]
fn a_tool_entry_names_what_finds_it() {
    for (category, entry) in entries() {
        if entry.status == Status::Tool {
            assert!(
                !entry.refs.is_empty(),
                "{category}/{} names no tool",
                entry.name
            );
        }
    }
}

#[test]
fn anything_not_a_plain_check_explains_itself() {
    for (category, entry) in entries() {
        if matches!(
            entry.status,
            Status::Gap | Status::OutOfScope | Status::Partial
        ) {
            assert!(
                !entry.note.trim().is_empty(),
                "{category}/{} has status {} and no note",
                entry.name,
                entry.status.as_str()
            );
        }
    }
}

#[test]
fn no_entry_claims_both_a_check_and_a_gap() {
    for (_, entry) in entries() {
        if entry.status == Status::Gap {
            assert!(entry.refs.is_empty(), "{}", entry.name);
        }
    }
}

#[test]
fn the_counts_add_up_and_coverage_is_the_majority_of_scope() {
    let counts = coverage_counts();
    assert_eq!(counts.total(), TOTAL_PUBLISHED_ISSUES);
    let in_scope = TOTAL_PUBLISHED_ISSUES
        - category_entries("Accessibility").len()
        - category_entries("AMP").len();
    let found = counts.check + counts.tool;
    assert!(
        found * 2 > in_scope,
        "found {found} of {in_scope} in-scope issues"
    );
}

// Pinned against `coverage_counts()`; a change here is a coverage change.
#[test]
fn counts_match_the_oracle() {
    let counts = coverage_counts();
    assert_eq!(counts.check, ORACLE_CHECK);
    assert_eq!(counts.tool, ORACLE_TOOL);
    assert_eq!(counts.partial, 0);
    assert_eq!(counts.gap, 0);
    assert_eq!(counts.out_of_scope, ORACLE_OUT_OF_SCOPE);
    assert_eq!(counts.get(Status::Check), ORACLE_CHECK);
}

// Pins the size of each category.
#[test]
fn categories_keep_the_oracle_order_and_sizes() {
    let names: Vec<&str> = categories().iter().map(|category| category.name).collect();
    assert_eq!(names, ORACLE_CATEGORY_ORDER);
    let sizes: Vec<(&str, usize)> = [
        ("Response Codes", 15),
        ("Page Titles", 9),
        ("Accessibility", 92),
        ("Search Console", 10),
        ("AMP", 16),
    ]
    .to_vec();
    for (name, size) in sizes {
        assert_eq!(category_entries(name).len(), size, "{name}");
    }
}

#[test]
fn network_payload_entry_does_not_overclaim_transfer_coverage() {
    let entry = category_entries("PageSpeed")
        .into_iter()
        .find(|entry| entry.name == "Avoid Enormous Network Payloads")
        .expect("entry exists");
    assert_eq!(entry.status, Status::Check);
    assert!(entry.refs.contains(&"HEAVY_NETWORK_PAYLOAD"));
    assert!(entry.note.contains("compressed transfer"));
    assert!(entry.note.contains("not modelled"));
}

// The four static-DOM rules name their check ids.
#[test]
fn static_accessibility_rules_name_their_check_ids() {
    let table = [
        (
            "WCAG 2.0 A - HTML Element Requires Lang Attribute",
            "A11Y_HTML_LANG_MISSING",
        ),
        (
            "WCAG 2.0 A - Form <input> Elements Require Labels",
            "A11Y_FORM_LABEL_MISSING",
        ),
        (
            "WCAG 2.0 A - Select Element Requires Accessible Name",
            "A11Y_FORM_LABEL_MISSING",
        ),
        (
            "WCAG 2.0 A - Images Require Alternate Text",
            "A11Y_IMAGE_ALT_MISSING",
        ),
    ];
    let accessibility = category_entries("Accessibility");
    for (name, code) in table {
        let entry = accessibility
            .iter()
            .find(|entry| entry.name == name)
            .expect("rule listed");
        assert_eq!(entry.status, Status::Check, "{name}");
        assert_eq!(entry.refs, [code], "{name}");
    }
    let declined = accessibility
        .iter()
        .filter(|entry| entry.status == Status::OutOfScope)
        .count();
    assert_eq!(declined, 88);
}

// Pairing rules and the structural rows answered by checks.
#[test]
fn amp_rows_map_to_their_check_ids() {
    let amp = category_entries("AMP");
    let find = |name: &str| {
        amp.iter()
            .find(|entry| entry.name == name)
            .copied()
            .expect("row listed")
    };
    assert_eq!(find("Non-200 Response").refs, ["AMP_NON_200"]);
    assert_eq!(
        find("Missing Canonical to Non-AMP").refs,
        ["AMP_MISSING_RETURN_LINK"]
    );
    assert_eq!(find("Missing Head Tag").refs, ["HEAD_MISSING"]);
    assert_eq!(
        find("Missing/Invalid Meta Viewport Tag").refs,
        ["VIEWPORT_MISSING", "INVALID_VIEWPORT"]
    );
    assert_eq!(find("Contains Disallowed HTML").status, Status::OutOfScope);
    assert_eq!(find("Other Validation Errors").status, Status::OutOfScope);
}

// A plain check entry carries no note; a note-bearing entry keeps it.
#[test]
fn entry_builders_set_only_the_fields_they_name() {
    let plain = Entry::check("X", &["ID"]);
    assert_eq!(plain.note, "");
    assert_eq!(plain.status, Status::Check);
    let noted = plain.note("why");
    assert_eq!(noted.note, "why");
    assert_eq!(Entry::gap("G").refs, &[] as &[&str]);
    assert_eq!(Entry::out_of_scope("O").status.as_str(), "out_of_scope");
}
