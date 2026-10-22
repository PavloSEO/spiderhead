"""Native Flesch reading ease over the retained content area (#1516).

The analyzer's readability checks used to need an SF export's columns; a
native crawl now computes the same numbers from its own retained content, and
the existing READABILITY_DIFFICULT / LONG_SENTENCES checks read them through
the same column names. Unmeasured is None, never zero.
"""

from __future__ import annotations

from seohead.checks.parser import parse_html
from seohead.checks.readability import READABILITY_VERSION, reading_ease
from seohead.crawl.collect import collect_urls
from seohead.crawl.evidence import build_evidence
from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import run_rules

_LONG = " ".join(
    "The considerable sophistication of contemporary information architecture "
    "frequently necessitates comprehensive methodological examination before "
    "implementation commences across distributed organizational environments."
    for _ in range(6)
)
_SHORT = "The cat sat on the mat. Dogs run fast. We saw it. Try this now. " * 6


def _page(text: str, title: str) -> str:
    return (
        f"<html><head><title>{title}</title></head><body><main><h1>{title}</h1>"
        f"<p>{text}</p></main></body></html>"
    )


class _FakeResponse:
    def __init__(self, text: str):
        self.text = text
        self.status_code = 200
        self.headers = {"content-type": "text/html"}


def _run_crawl(mapping):
    crawl_result = collect_urls(
        list(mapping), fetcher=lambda url: mapping[url], sleeper=lambda _s: None
    )
    evidence = build_evidence(crawl_result)
    exports = LoadedExports()
    exports.frames.update(evidence["frames"])
    exports.found = list(evidence["found"])
    exports.missing = list(evidence["missing"])
    ctx = AuditContext(exports, load_config(None))
    ctx.skip_unsupported(set(exports.frames))
    run_rules(ctx)
    return ctx, crawl_result


def _fired(ctx) -> dict[str, set[str]]:
    out: dict[str, set[str]] = {}
    for issue in ctx.issues:
        out.setdefault(issue.check, set()).add(issue.target_url)
    return out


def test_long_sentences_score_harder_than_short_ones():
    hard = reading_ease(_LONG)
    easy = reading_ease(_SHORT)
    assert hard["avg_words_per_sentence"] > easy["avg_words_per_sentence"]
    assert hard["flesch"] < easy["flesch"]
    assert hard["readability"] in ("very difficult", "difficult")
    assert easy["readability"] in ("very easy", "easy")
    assert hard["version"] == easy["version"] == READABILITY_VERSION


def test_text_without_letters_is_unmeasured_not_zero():
    assert reading_ease("123 456 ... !!!") is None
    assert reading_ease("") is None


def test_parser_carries_the_metrics_from_the_content_area():
    parsed = parse_html(_page(_LONG, "Long"), "https://example.test/long")
    assert parsed["readability"]["sentence_count"] == 6
    assert parsed["readability"]["avg_words_per_sentence"] > 15


def test_native_crawl_feeds_the_existing_readability_checks():
    ctx, result = _run_crawl(
        {
            "https://example.test/hard": _FakeResponse(_page(_LONG, "Hard")),
            "https://example.test/easy": _FakeResponse(_page(_SHORT, "Easy")),
        }
    )
    fired = _fired(ctx)
    # Flesch 0 sits under the default readability_flesch_min of 30; the easy
    # page sits far above it. LONG_SENTENCES keeps its own 25-word threshold.
    assert "https://example.test/hard" in fired.get("READABILITY_DIFFICULT", set())
    assert "https://example.test/easy" not in fired.get("READABILITY_DIFFICULT", set())
    # The record stores the metrics, unmeasured never zero.
    hard = next(page for page in result.pages if page.url.endswith("/hard"))
    assert hard.sentence_count == 6
    assert hard.avg_words_per_sentence > 15
    assert hard.flesch is not None and hard.flesch < 50
    assert hard.readability in ("very difficult", "difficult")
