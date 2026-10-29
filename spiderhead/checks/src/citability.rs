//! Citability score for generative and answer-engine optimization.
//!
//! Four dimensions contribute up to 25 points each: answer blocks (self-contained
//! 20-200 word paragraphs), self-containment (absence of context-dependent
//! transitions), statistical density (numbers and evidence markers per 100 words)
//! and structure quality (headings, lists, TL;DR markers, readable paragraphs).
//! The scorer is a pure function of the text.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

/// Phrases that make a paragraph depend on its surroundings (Russian and English).
const CONTEXT_PHRASES: [&str; 25] = [
    "как упоминалось",
    "как сказано",
    "как отмечалось",
    "как говорилось",
    "см. выше",
    "смотри выше",
    "выше мы",
    "ранее мы",
    "как мы уже",
    "as mentioned",
    "as noted",
    "as discussed",
    "see above",
    "see below",
    "as we",
    "earlier we",
    "previously",
    "however",
    "thus",
    "therefore",
    "однако",
    "тем не менее",
    "следовательно",
    "таким образом",
    "итак",
];

const TARGET_BAND: std::ops::RangeInclusive<usize> = 40..=60;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| unreachable!("static pattern {pattern}"))
}

static BAD_START: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\A(?:я|мы|ты|вы|он|она|оно|они|это|этот|эта|эти|тот|та|те|такой|здесь|там|i|we|you|they|it|this|that|these|those|here|there|therefore|thus|so)\b",
    )
});
static QUESTION_START: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\A(?:почему|зачем|как|что|кто|где|когда|какой|why|how|what|who|where|when|which)\b.*\?\s*\z",
    )
});
static STAT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\d+(?:[.,]\d+)?\s?%|\b\d{4}\b|\b\d+(?:[.,]\d+)?\b"));
static EVIDENCE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)согласно|исследован|исследованию|по данным|статистик|опрос|доказыва|according to|research|study|studies|survey|data shows|evidence|report",
    )
});
static ORIGINAL_DATA: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)наш(?:его|е|и|а)? (?:исследован|опрос|анализ|тест|эксперимент|датасет|данные)|мы (?:проанализиров|опраш|провер|измер|собрал|выяснил)|собственные данные|первичные данные|our (?:study|survey|data|research|analysis|test|experiment|benchmark|dataset)|we (?:analyz|surveyed|tested|measured|polled|found that|collected)|internal data|proprietary data|first-party data",
    )
});
static SUPERLATIVE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:самый|наилучш|крупнейш|ведущ|номер один|№\s?1|эксперты утверждают|все знают|studies show|research shows|experts agree|everyone knows)|\b(?:the )?(?:best|fastest|most|largest|leading|number one|#1|cheapest)\b",
    )
});
// the reference non-multiline `^` only anchors the start of the whole text here.
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\A\s{0,3}#{1,6}\s+\S|\A\s{0,3}<h[1-6]\b"));
static LIST: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s{0,3}(?:[-*+]|\d+[.)])\s+\S"));
static TLDR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\btl;?dr\b|кратко|основное|главное|итог"));
static Q_HEADING_WORD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\A(?:как|что|почему|зачем|кто|где|когда|какой|сколько|how|what|why|when|where|who|which|can|is)\b",
    )
});
static DECLARATIVE_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\A(?:как\s+\w*(?:ть|ти|чь)\b|how\s+to\b|what\s+(?:we|you|i|our|they|this|that|the)\b)")
});
static HEADING_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?im)^[ \t]{0,3}(?:#{1,6}\s+\S|<h[1-6]\b)"));
static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"<[^>]+>"));
static PARAGRAPH_BREAK: LazyLock<Regex> = LazyLock::new(|| re(r"\n\s*\n"));
static WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\w+"));
static MD_HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"\A#{1,6}\s+(.+)"));
static HTML_HEADING: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?is)\A<h[1-6]\b[^>]*>(.*?)</h[1-6]>"));

fn paragraphs(text: &str) -> Vec<&str> {
    PARAGRAPH_BREAK
        .split(text)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

fn word_count(text: &str) -> usize {
    WORD.find_iter(text).count()
}

/// Whether a paragraph is a self-contained 20-200 word answer.
fn is_answer_block(paragraph: &str) -> bool {
    let words = word_count(paragraph);
    if !(20..=200).contains(&words) {
        return false;
    }
    let lower = paragraph.to_lowercase();
    if CONTEXT_PHRASES.iter().any(|phrase| lower.contains(phrase)) {
        return false;
    }
    let first_line = paragraph.split('\n').next().unwrap_or_default();
    !BAD_START.is_match(first_line) && !QUESTION_START.is_match(first_line)
}

fn strip_tags(text: &str) -> String {
    TAG.replace_all(text, " ").into_owned()
}

/// A leading heading line separated from the rest of a text block.
fn split_heading_body(chunk: &str) -> (Option<String>, &str) {
    let (first_line, rest) = chunk.split_once('\n').unwrap_or((chunk, ""));
    let line = first_line.trim();
    if let Some(found) = MD_HEADING.captures(line) {
        let heading = found[1].trim().trim_end_matches('#').trim().to_owned();
        return (Some(heading), rest);
    }
    if let Some(found) = HTML_HEADING.captures(line) {
        return (Some(strip_tags(&found[1]).trim().to_owned()), rest);
    }
    (None, chunk)
}

/// `(heading | None, body)` blocks: every heading starts a block that runs to the next.
fn heading_blocks(text: &str) -> Vec<(Option<String>, &str)> {
    let starts: Vec<usize> = HEADING_LINE.find_iter(text).map(|m| m.start()).collect();
    let Some(&first) = starts.first() else {
        return vec![(None, text)];
    };
    let mut blocks = Vec::new();
    if first > 0 {
        blocks.push((None, &text[..first]));
    }
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(text.len());
        blocks.push(split_heading_body(&text[*start..end]));
    }
    blocks
}

fn is_question_heading(heading: &str) -> bool {
    let heading = heading.trim();
    if heading.is_empty() {
        return false;
    }
    (heading.ends_with('?') || Q_HEADING_WORD.is_match(heading))
        && !DECLARATIVE_HEADING.is_match(heading)
}

fn first_answer_words(body: &str) -> usize {
    paragraphs(body)
        .into_iter()
        .map(|para| strip_tags(para).trim().to_owned())
        .find(|clean| !clean.is_empty())
        .map_or(0, |clean| word_count(&clean))
}

/// `(question headings, with a direct answer, answers in the target band)`.
fn count_question_answers(text: &str) -> (usize, usize, usize) {
    let (mut questions, mut answered, mut in_band) = (0, 0, 0);
    for (heading, body) in heading_blocks(text) {
        let Some(heading) = heading.filter(|h| is_question_heading(h)) else {
            continue;
        };
        let _ = heading;
        questions += 1;
        let words = first_answer_words(body);
        if words > 0 {
            answered += 1;
            if TARGET_BAND.contains(&words) {
                in_band += 1;
            }
        }
    }
    (questions, answered, in_band)
}

/// the reference `round(value, 1)`: correctly rounded, ties to even on the exact value.
fn round1(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}

/// Citability of `text` from 0 to 100, without network access.
pub fn score_citability(text: &str) -> Value {
    if text.trim().is_empty() {
        return json!({"ok": false, "error": "Text is empty"});
    }
    let paragraph_list = paragraphs(text);
    let words = word_count(text);
    if words == 0 {
        return json!({"ok": false, "error": "No words are available for scoring"});
    }
    let (question_headings, with_answer, in_band) = count_question_answers(text);

    let good = paragraph_list.iter().filter(|p| is_answer_block(p)).count();
    let answer_blocks = if paragraph_list.is_empty() {
        0.0
    } else {
        round1(25.0 * (good as f64 / paragraph_list.len() as f64))
    };

    let lower = text.to_lowercase();
    let context_hits: usize = CONTEXT_PHRASES
        .iter()
        .map(|phrase| lower.matches(phrase).count())
        .sum();
    let penalty = context_hits as f64 / paragraph_list.len().max(1) as f64;
    let self_containment = round1(25.0 * (1.0 - penalty).max(0.0));

    let stats = STAT.find_iter(text).count();
    let evidence = EVIDENCE.find_iter(text).count();
    let per_100 = (stats + evidence) as f64 / words as f64 * 100.0;
    let mut statistical_density = round1((per_100 / 8.0 * 25.0).min(25.0));
    let original_data = ORIGINAL_DATA.is_match(text);
    if original_data && statistical_density > 0.0 {
        statistical_density = round1((statistical_density + 3.0).min(25.0));
    }

    let (has_headings, has_lists, has_tldr) = (
        HEADING.is_match(text),
        LIST.is_match(text),
        TLDR.is_match(text),
    );
    let mut structure = 0.0;
    if has_headings {
        structure += 8.0;
    }
    if has_lists {
        structure += 7.0;
    }
    if has_tldr {
        structure += 5.0;
    }
    if !paragraph_list.is_empty() {
        let mut counts: Vec<usize> = paragraph_list.iter().map(|p| word_count(p)).collect();
        counts.sort_unstable();
        if (30..=150).contains(&counts[counts.len() / 2]) {
            structure += 5.0;
        }
    }
    if in_band >= 1 {
        structure += 5.0;
    }
    let structure_quality = round1(f64::min(25.0, structure));

    let total = round1(answer_blocks + self_containment + statistical_density + structure_quality);
    let grade = match total {
        t if t >= 75.0 => "A",
        t if t >= 55.0 => "B",
        t if t >= 35.0 => "C",
        t if t >= 15.0 => "D",
        _ => "F",
    };
    json!({
        "ok": true,
        "score": total,
        "grade": grade,
        "word_count": words,
        "paragraphs": paragraph_list.len(),
        "dimensions": {
            "answer_blocks": answer_blocks,
            "self_containment": self_containment,
            "statistical_density": statistical_density,
            "structure_quality": structure_quality,
        },
        "signals": {
            "answer_block_paragraphs": good,
            "context_phrase_hits": context_hits,
            "statistical_markers": stats,
            "evidence_markers": evidence,
            "original_data": original_data,
            "superlative_claims": SUPERLATIVE.find_iter(text).count(),
            "has_headings": has_headings,
            "has_lists": has_lists,
            "has_tldr": has_tldr,
            "question_headings": question_headings,
            "questions_with_direct_answer": with_answer,
            "answers_in_target_band": in_band,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_refused() {
        assert_eq!(score_citability("  \n")["error"], "Text is empty");
    }

    #[test]
    fn statistics_and_a_question_answer_pair_score() {
        let report = score_citability("Short text. Another sentence with 42% growth in 2024.");
        assert_eq!(report["score"], 50.0);
        assert_eq!(report["grade"], "C");
        assert_eq!(report["signals"]["statistical_markers"], 2);
    }

    #[test]
    fn question_headings_count_direct_answers() {
        let answer = vec!["word"; 45].join(" ");
        let text = format!("## What is a canonical?\n\n{answer}\n");
        let report = score_citability(&text);
        assert_eq!(report["signals"]["question_headings"], 1);
        assert_eq!(report["signals"]["answers_in_target_band"], 1);
    }
}
