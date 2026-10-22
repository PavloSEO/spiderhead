"""Rejected link targets leave evidence instead of vanishing (issue #1530).

Link parsing drops an href it cannot turn into a fetchable target -- unparsable
syntax, a non-http(s) scheme -- without a trace. These tests cover the three
layers that make the drop visible when the crawl asks for it: the pure parser
fact, its survival through the crawl record and the analyzer frame, and the
registry check over it. The recording is opt-in
(``discovery.record_invalid_targets``), so off means the evidence
never existed, which is a skip, never a clean page.
"""

from __future__ import annotations

from seohead.checks.parser import parse_html
from seohead.crawl.collect import collect_urls
from seohead.crawl.evidence import build_evidence
from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import run_rules

_PAGE = """<html><head><title>{title}</title></head><body>
<main><h1>{title}</h1><p>Enough body text to be a real page. {extra}</p>
{links}</main></body></html>"""

_BROKEN_LINKS = """
<a href="https://example.com/real">A real link</a>
<a href="http://[invalid">Truncated IPv6-ish href</a>
<a href="mailto:hi@example.com">A mail contact</a>
<a href="#anchor">In-page anchor</a>
<a name="bookmark">No href at all</a>"""

_CLEAN_LINKS = '<a href="https://example.com/real">A real link</a>'


class _FakeResponse:
    def __init__(self, text: str):
        self.text = text
        self.status_code = 200
        self.headers = {"content-type": "text/html"}


def _page(title: str, links: str) -> str:
    return _PAGE.format(title=title, links=links, extra=" ".join([title] * 40))


def _run_crawl(mapping, parse_options=None):
    crawl_result = collect_urls(
        list(mapping),
        fetcher=lambda url: mapping[url],
        sleeper=lambda _s: None,
        parse_options=parse_options,
    )
    evidence = build_evidence(crawl_result)
    exports = LoadedExports()
    exports.frames.update(evidence["frames"])
    exports.found = list(evidence["found"])
    exports.missing = list(evidence["missing"])
    ctx = AuditContext(exports, load_config(None))
    ctx.skip_unsupported(set(exports.frames))
    run_rules(ctx)
    return ctx


def _fired(ctx) -> dict[str, set[str]]:
    out: dict[str, set[str]] = {}
    for issue in ctx.issues:
        out.setdefault(issue.check, set()).add(issue.target_url)
    return out


def _details(ctx, check_id, url):
    return next(i.details for i in ctx.issues if i.check == check_id and i.target_url == url)


def _skips(ctx, check_id) -> list[str]:
    return [s.reason for s in ctx.skipped if s.id == check_id]


# -- pure parser facts --------------------------------------------------------


def test_invalid_targets_are_recorded_with_their_reason_when_switched_on():
    parsed = parse_html(
        _page("Broken", _BROKEN_LINKS), "https://example.com/broken", {"invalid_link_targets": True}
    )
    assert parsed["invalid_link_targets"] == [
        {"raw_href": "http://[invalid", "reason": "unparsable_syntax"},
        {"raw_href": "mailto:hi@example.com", "reason": "unsupported_scheme"},
    ]
    # Valid links are unaffected, and an in-page anchor or a missing href is
    # absence, not a defect -- neither is recorded.
    assert [link["href"] for link in parsed["links"]] == ["https://example.com/real"]


def test_invalid_targets_are_absent_unless_the_crawl_asks_for_them():
    parsed = parse_html(_page("Broken", _BROKEN_LINKS), "https://example.com/broken")
    assert "invalid_link_targets" not in parsed


def test_a_clean_page_records_an_empty_list_not_an_absence():
    parsed = parse_html(
        _page("Clean", _CLEAN_LINKS), "https://example.com/clean", {"invalid_link_targets": True}
    )
    assert parsed["invalid_link_targets"] == []


# -- registry check, through a native crawl ---------------------------------


def test_invalid_link_target_fires_for_exactly_the_broken_page():
    ctx = _run_crawl(
        {
            "https://example.com/broken": _FakeResponse(_page("Broken", _BROKEN_LINKS)),
            "https://example.com/clean": _FakeResponse(_page("Clean", _CLEAN_LINKS)),
        },
        parse_options={"invalid_link_targets": True},
    )
    assert _fired(ctx).get("INVALID_LINK_TARGET", set()) == {"https://example.com/broken"}
    assert _details(ctx, "INVALID_LINK_TARGET", "https://example.com/broken") == {
        "count": 2,
        "first_targets": [
            {"raw_href": "http://[invalid", "reason": "unparsable_syntax"},
            {"raw_href": "mailto:hi@example.com", "reason": "unsupported_scheme"},
        ],
    }


def test_invalid_link_target_skips_honestly_when_recording_was_off():
    ctx = _run_crawl({"https://example.com/broken": _FakeResponse(_page("Broken", _BROKEN_LINKS))})
    assert "INVALID_LINK_TARGET" not in _fired(ctx)
    assert _skips(ctx, "INVALID_LINK_TARGET") == [
        "invalid link target recording was off for this crawl "
        "(discovery.record_invalid_targets)"
    ]


def test_invalid_link_target_measured_clean_page_fires_nothing():
    ctx = _run_crawl(
        {"https://example.com/clean": _FakeResponse(_page("Clean", _CLEAN_LINKS))},
        parse_options={"invalid_link_targets": True},
    )
    assert "INVALID_LINK_TARGET" not in _fired(ctx)
    assert _skips(ctx, "INVALID_LINK_TARGET") == []
