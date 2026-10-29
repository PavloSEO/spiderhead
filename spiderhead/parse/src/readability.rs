//! Flesch Reading Ease over a page's retained content area.
//! English vowel-run syllable heuristic; sentence splitting and word tests are Unicode-aware.
//! Word tests count letters only.

use crate::models::Readability;

/// Identifier of this exact computation: formula, syllable heuristic and band boundaries.
pub const READABILITY_VERSION: &str = "flesch-reading-ease.v1";

/// Band upper bounds and labels, ascending.
pub const BANDS: [(f64, &str); 7] = [
    (30.0, "very difficult"),
    (50.0, "difficult"),
    (60.0, "fairly difficult"),
    (70.0, "standard"),
    (80.0, "fairly easy"),
    (90.0, "easy"),
    (101.0, "very easy"),
];

fn is_word_char(c: char) -> bool {
    use unicode_general_category::{get_general_category as cat, GeneralCategory as G};
    matches!(
        cat(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
            | G::LetterNumber
            | G::OtherNumber
    )
}

fn has_letter(token: &str) -> bool {
    token.chars().any(is_word_char)
}

/// One syllable per vowel run, minimum one.
fn syllables(word: &str) -> usize {
    let mut runs = 0;
    let mut in_run = false;
    for c in word.chars() {
        // Case-insensitive vowel matching also covers dotless and dotted Turkish I.
        let vowel = matches!(
            c.to_ascii_lowercase(),
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | '\u{131}' | '\u{130}'
        );
        if vowel && !in_run {
            runs += 1;
        }
        in_run = vowel;
    }
    runs.max(1)
}

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{2026}')
}

/// Sentence split: a run of `.`, `!`, `?` or `…` followed by whitespace, or at end of text.
fn split_sentences(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut pieces = Vec::new();
    let (mut piece_start, mut i) = (0, 0);
    while i < chars.len() {
        if !is_terminator(chars[i].1) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && is_terminator(chars[j].1) {
            j += 1;
        }
        let mut k = j;
        while k < chars.len() && (crate::py_str::is_py_space(chars[k].1) || chars[k].1 == '\u{a0}')
        {
            k += 1;
        }
        let end_byte = |idx: usize| chars.get(idx).map_or(text.len(), |c| c.0);
        // A terminator at the very end also matches before one trailing newline.
        let at_end = j == chars.len() || (j + 1 == chars.len() && chars[j].1 == '\n');
        if k > j {
            pieces.push(&text[piece_start..chars[i].0]);
            piece_start = end_byte(k);
            i = k;
        } else if at_end {
            pieces.push(&text[piece_start..chars[i].0]);
            piece_start = end_byte(j);
            i = j;
        } else {
            i = j;
        }
    }
    pieces.push(&text[piece_start..]);
    pieces
}

/// Round to `ndigits` decimals, ties to even on the exact binary value.
fn py_round(value: f64, ndigits: usize) -> f64 {
    format!("{value:.ndigits$}").parse().unwrap_or(value)
}

/// Sentence/word/syllable counts, Flesch score and band; `None` when no word has a letter.
pub fn reading_ease(text: &str) -> Option<Readability> {
    let words: Vec<&str> = text
        .split(crate::py_str::is_py_space)
        .filter(|t| has_letter(t))
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut sentences = split_sentences(text)
        .into_iter()
        .filter(|c| has_letter(c))
        .count();
    if sentences == 0 {
        sentences = 1;
    }
    let syllable_total: usize = words.iter().map(|w| syllables(w)).sum();
    let avg_words = words.len() as f64 / sentences as f64;
    let syllables_per_word = syllable_total as f64 / words.len() as f64;
    let score = 206.835 - 1.015 * avg_words - 84.6 * syllables_per_word;
    let flesch = score.clamp(0.0, 100.0);
    let band = BANDS
        .iter()
        .find(|(bound, _)| flesch < *bound)
        .map_or("very easy", |(_, label)| label);
    Some(Readability {
        version: READABILITY_VERSION.to_owned(),
        sentence_count: sentences as u64,
        avg_words_per_sentence: py_round(avg_words, 2),
        flesch: py_round(flesch, 1),
        readability: band.to_owned(),
    })
}
