//!
//! The registry check tests (crawl -> evidence -> rules pipeline) need the crawl and
//! checks crates and are deferred; see the slice report.

use spiderhead_parse::parser::content::count_lorem_ipsum;

#[test]
fn full_passage_counts_one_occurrence() {
    let text = "Some real copy. Lorem ipsum dolor sit amet, consectetur adipiscing elit.";
    assert_eq!(count_lorem_ipsum(text), 1);
}

#[test]
fn two_occurrences_are_both_counted() {
    let text = "Lorem ipsum dolor sit amet. ".repeat(2);
    assert_eq!(count_lorem_ipsum(&text), 2);
}

#[test]
fn incidental_mention_of_the_word_lorem_alone_does_not_match() {
    assert_eq!(
        count_lorem_ipsum("Meet Lorem, our new ipsum-themed typography plugin."),
        0
    );
}

#[test]
fn empty_content_counts_zero() {
    assert_eq!(count_lorem_ipsum(""), 0);
    assert_eq!(
        count_lorem_ipsum("Perfectly ordinary page content with nothing amiss."),
        0
    );
}
