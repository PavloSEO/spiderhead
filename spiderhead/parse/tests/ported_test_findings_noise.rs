//! Ports of the the reference suite tests that need only the duplicate detector.
//! The structured-data, mixed-content, template-grouping and repeated-link tests read the
//! audit context of the checks and sf-import crates, which are not ported yet, and are deferred.

use spiderhead_parse::duplicate::{find_duplicates, DuplicateItem, DuplicateOptions};

fn item(id: &str, text: &str) -> DuplicateItem {
    DuplicateItem {
        id: id.into(),
        text: text.into(),
        indexable: true,
    }
}

#[test]
fn duplicate_check_with_no_compared_pairs_says_unmeasured() {
    let result = find_duplicates(
        &[item(
            "a",
            "Recipes for a quick dinner with fresh fish and vegetables.",
        )],
        &DuplicateOptions::default(),
    )
    .expect("find_duplicates");
    assert!(result.ok);
    assert!(!result.measured);
    assert_eq!(result.candidate_pairs_checked, 0);
    assert!(result
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("unmeasured"));
}

#[test]
fn duplicate_check_with_an_empty_corpus_says_unmeasured() {
    let result = find_duplicates(&[], &DuplicateOptions::default()).expect("find_duplicates");
    assert!(!result.measured);
}

#[test]
fn duplicate_check_with_a_compared_pair_is_measured() {
    let text =
        "Search engine optimization is the process of improving the quality and quantity of \
                website traffic from search engines and it targets unpaid organic results.";
    let result = find_duplicates(
        &[item("a", text), item("b", text)],
        &DuplicateOptions::default(),
    )
    .expect("find_duplicates");
    assert!(result.measured);
    assert!(result.reason.is_none());
}
