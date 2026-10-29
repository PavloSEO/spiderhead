//! Rendering-escalation verdicts that depend on how much text a raw page carries.

use serde_json::Value;

/// A raw page with fewer words than this is too thin to stand on its own without JavaScript.
pub const EMPTY_BODY_WORDS: i64 = 50;

/// Whether a raw snapshot is too thin to trust as the whole picture.
///
/// Feeds the automatic escalation policy: a sampled pattern whose raw page carries
/// little text is escalated even when raw and rendered agree. A missing, null or
/// non-numeric `words` count reads as zero.
pub fn little_text(snapshot: &Value) -> bool {
    let words = snapshot
        .get("words")
        .and_then(|w| w.as_i64().or_else(|| w.as_f64().map(|f| f as i64)))
        .unwrap_or(0);
    words < EMPTY_BODY_WORDS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn little_text_uses_the_empty_body_threshold() {
        for (words, thin) in [
            (0, true),
            (EMPTY_BODY_WORDS - 1, true),
            (EMPTY_BODY_WORDS, false),
        ] {
            assert_eq!(little_text(&json!({ "words": words })), thin, "{words}");
        }
    }

    #[test]
    fn little_text_treats_a_missing_count_as_thin() {
        assert!(little_text(&json!({})));
    }
}
