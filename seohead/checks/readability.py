"""Flesch Reading Ease over a page's retained content area (issue #1516).

The formula is the classical Flesch Reading Ease score (Flesch 1948, as
reprinted in countless style guides):

    206.835 - 1.015 * (words / sentences) - 84.6 * (syllables / words)

clamped to 0..100, with the standard band table:

    0-30   very difficult    60-70  standard
    30-50  difficult         70-80  fairly easy
    50-60  fairly difficult  80-90  easy
                             90-100 very easy

``READABILITY_VERSION`` names this exact computation (formula + syllable
heuristic + band boundaries): a stored score is interpretable only together
with the rule that produced it, and the heuristic below will move as real
pages expose its edges.

Scope, stated rather than hidden: syllables are counted by English vowel
groups, the way every offline implementation of this formula counts them. The
score is meaningful for English text, directionally useful for languages with
a similar vowel pattern, and honest about neither for scripts without Latin
vowels -- the band then describes the formula's answer, not the reader's
experience. Nothing here stops a caller from computing a better score
elsewhere and naming it with its own version.

Sentences are terminal-punctuation runs (``.``, ``!``, ``?``, ``…``, and the
ellipsis character) that end a non-empty stretch of words; a text with no
terminal punctuation at all is one sentence, because "unmeasured" is reserved
for text with nothing to measure (no word with a letter), which returns
``None`` -- never a zero that reads as "measured, very easy".
"""

from __future__ import annotations

import re
from typing import Any

READABILITY_VERSION = "flesch-reading-ease.v1"

# Bands: (upper bound exclusive, label), checked in order.
_BANDS: tuple[tuple[float, str], ...] = (
    (30.0, "very difficult"),
    (50.0, "difficult"),
    (60.0, "fairly difficult"),
    (70.0, "standard"),
    (80.0, "fairly easy"),
    (90.0, "easy"),
    (101.0, "very easy"),
)

_SENTENCE_SPLIT = re.compile(r"[.!?…]+[\s\xa0]+|[.!?…]+$")
_WORD = re.compile(r"[^\W\d_]", re.UNICODE)  # at least one letter
_VOWEL_RUN = re.compile(r"[aeiouy]+", re.IGNORECASE)


def _syllables(word: str) -> int:
    """English vowel-group heuristic: one syllable per vowel run, minimum one."""
    count = len(_VOWEL_RUN.findall(word))
    # A silent trailing 'e' the group heuristic overcounts ("table" -> 2) is a
    # known, accepted approximation; the version marker covers it.
    return count if count else 1


def reading_ease(text: str) -> dict[str, Any] | None:
    """Sentence/word/syllable counts, Flesch score and band for one content text.

    ``None`` when the text has no word with a letter: nothing was measured, and
    the caller must store unmeasured rather than a zero that would read as a
    measured, maximally easy page.
    """
    words = [token for token in (text or "").split() if _WORD.search(token)]
    if not words:
        return None
    sentences = [chunk for chunk in _SENTENCE_SPLIT.split(text or "") if _WORD.search(chunk)] or [
        text or " "
    ]
    syllables = sum(_syllables(word) for word in words)
    avg_words_per_sentence = len(words) / len(sentences)
    syllables_per_word = syllables / len(words)
    score = 206.835 - 1.015 * avg_words_per_sentence - 84.6 * syllables_per_word
    flesch = max(0.0, min(100.0, score))
    band = next(label for bound, label in _BANDS if flesch < bound)
    return {
        "version": READABILITY_VERSION,
        "sentence_count": len(sentences),
        "avg_words_per_sentence": round(avg_words_per_sentence, 2),
        "flesch": round(flesch, 1),
        "readability": band,
    }
