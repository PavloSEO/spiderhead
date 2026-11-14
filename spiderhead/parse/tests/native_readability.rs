//! native-crawl case needs the crawl and checks crates and is deferred; see the port report.

use spiderhead_parse::parse_html;
use spiderhead_parse::readability::{reading_ease, READABILITY_VERSION};
use spiderhead_parse::types::ParseOptions;

const LONG_SENTENCE: &str = "The considerable sophistication of contemporary information architecture frequently necessitates comprehensive methodological examination before implementation commences across distributed organizational environments.";

const SHORT_SENTENCES: &str = "The cat sat on the mat. Dogs run fast. We saw it. Try this now. ";

fn long_text() -> String {
    [LONG_SENTENCE; 6].join(" ")
}

fn short_text() -> String {
    SHORT_SENTENCES.repeat(6)
}

fn page(text: &str, title: &str) -> String {
    format!(
        "<html><head><title>{title}</title></head><body><main><h1>{title}</h1><p>{text}</p></main></body></html>"
    )
}

#[test]
fn long_sentences_score_harder_than_short_ones() {
    let hard = reading_ease(&long_text()).expect("measured");
    let easy = reading_ease(&short_text()).expect("measured");
    assert!(hard.avg_words_per_sentence > easy.avg_words_per_sentence);
    assert!(hard.flesch < easy.flesch);
    assert!(["very difficult", "difficult"].contains(&hard.readability.as_str()));
    assert!(["very easy", "easy"].contains(&easy.readability.as_str()));
    assert_eq!(hard.version, READABILITY_VERSION);
    assert_eq!(easy.version, READABILITY_VERSION);
}

#[test]
fn text_without_letters_is_unmeasured_not_zero() {
    assert!(reading_ease("123 456 ... !!!").is_none());
    assert!(reading_ease("").is_none());
}

#[test]
fn parser_carries_the_metrics_from_the_content_area() {
    let parsed = parse_html(
        &page(&long_text(), "Long"),
        "https://example.test/long",
        &ParseOptions::default(),
    )
    .expect("parse_html");
    let readability = parsed.readability.expect("readability measured");
    assert_eq!(readability.sentence_count, 6);
    assert!(readability.avg_words_per_sentence > 15.0);
}
