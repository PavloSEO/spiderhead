"""Heading order and heading region (issue #632).

The registry's eight heading checks read h1/h1_2/h2 -- headings as an unordered
set. A page whose DOM order is ``H2, H2, ..., H1, H2`` has exactly one H1 and
plenty of H2s, so every one of them is satisfied, and so is a page whose H2s are
all menu labels in its masthead. Both defects need the outline as a *sequence*
that also says where on the page each heading sits.

These tests cover the three layers that had to exist for the checks to be
writable at all: the pure parser fact (``parser.heading_outline``), its survival
through the storage schema, and the two registry checks over it -- through the
same native-crawl path ``test_element_position.py`` uses for the other evidence
no Screaming Frog export carries.
"""

from __future__ import annotations

import json

import pytest

from seohead.checks.parser import heading_outline, parse_html
from seohead.crawl.collect import collect_urls
from seohead.crawl.evidence import build_evidence
from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import run_rules
from seohead.storage import ScanError, import_run, open_scan
from tests.test_scan_artifact import BUILD
from tests.test_scan_artifact import legacy_run as legacy_run

# -- pure parser facts --------------------------------------------------------

# The live shape issue #632 was found on: an article template whose masthead
# carries the headings of other magazines, all of them before the article's H1.
CHROME_HEAVY = """
<html><body>
  <header class="site-header"><h2>Magazine One</h2><h2>Magazine Two</h2></header>
  <nav><h3>Sections</h3></nav>
  <aside><h4>Also read</h4></aside>
  <main><h1>The article itself</h1><h2>A real section</h2></main>
  <footer><h2>About the publisher</h2></footer>
</body></html>
"""


def _levels_texts(outline):
    return [(item["level"], item["text"]) for item in outline]


def test_the_outline_is_document_order_not_grouped_by_level():
    parsed = parse_html(CHROME_HEAVY, "https://example.com/article")
    assert _levels_texts(parsed["heading_outline"]) == [
        (2, "Magazine One"),
        (2, "Magazine Two"),
        (3, "Sections"),
        (4, "Also read"),
        (1, "The article itself"),
        (2, "A real section"),
        (2, "About the publisher"),
    ]
    # The grouped view is unchanged and still cannot answer the same question.
    assert parsed["headings"]["h1"] == ["The article itself"]


def test_each_heading_carries_the_link_position_taxonomy_region():
    regions = [
        item["region"] for item in parse_html(CHROME_HEAVY, "https://x.test/")["heading_outline"]
    ]
    assert regions == ["header", "header", "nav", "sidebar", "content", "content", "footer"]


def test_a_page_with_no_landmark_and_no_matching_rule_reports_no_region():
    """Not "content": with the content root falling back to the whole <body>,
    "not chrome" would be true of every heading on the page and would say
    nothing at all."""
    html = "<html><body><div class='wrap'><h2>Early</h2><h1>Subject</h1></div></body></html>"
    outline = parse_html(html, "https://example.com/x")["heading_outline"]
    assert [item["region"] for item in outline] == ["", ""]


def test_a_site_specific_rule_places_a_heading_the_default_rules_miss():
    html = "<html><body><div class='mega'><h2>Menu</h2></div><main><h1>Subject</h1></main></body></html>"
    outline = parse_html(
        html,
        "https://example.com/x",
        {"link_position_rules": [{"position": "nav", "selector": ".mega"}]},
    )["heading_outline"]
    assert [(item["text"], item["region"]) for item in outline] == [
        ("Menu", "nav"),
        ("Subject", "content"),
    ]


def test_inert_and_textless_headings_are_left_out_exactly_as_the_grouping_leaves_them():
    html = """<html><body><main>
    <template><h2>Never rendered</h2></template>
    <h2>   </h2>
    <h1>Only real heading</h1>
    </main></body></html>"""
    parsed = parse_html(html, "https://example.com/x")
    assert _levels_texts(parsed["heading_outline"]) == [(1, "Only real heading")]
    assert "h2" not in parsed["headings"]


def test_hidden_and_closed_dialog_headings_are_left_out_of_the_outline():
    html = """<html><body><main>
    <h1>Visible page</h1><h2>Visible section</h2>
    <dialog><h2>Call us</h2><h4>Phone form</h4></dialog>
    <section hidden><h2>Hidden promotion</h2><h4>Hidden detail</h4></section>
    <aside aria-hidden="true"><h2>Invisible helper</h2></aside>
    </main></body></html>"""
    assert _levels_texts(parse_html(html, "https://example.com/x")["heading_outline"]) == [
        (1, "Visible page"),
        (2, "Visible section"),
    ]


def test_outline_extraction_can_be_switched_off_with_the_headings_option():
    parsed = parse_html(CHROME_HEAVY, "https://example.com/x", {"headings": False})
    assert parsed["heading_outline"] == []


def test_heading_outline_is_callable_on_its_own_tree():
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(CHROME_HEAVY, features="lxml")
    assert len(heading_outline(soup)) == 7


# -- registry checks, through a native crawl ---------------------------------


class _FakeResponse:
    def __init__(self, text: str):
        self.text = text
        self.status_code = 200
        self.headers = {"content-type": "text/html"}


def _page(body: str) -> str:
    return body + ("Enough body text to be a real page. " * 40)


_BAD_PAGE = f"""<html><head><title>An article behind its own masthead</title></head><body>
<header class="site-header"><h2>Magazine One</h2><h2>Magazine Two</h2></header>
<main><h1>The article itself</h1><h2>A real section</h2><p>{_page("")}</p></main>
<footer><h2>About the publisher</h2></footer></body></html>"""

_CLEAN_PAGE = f"""<html><head><title>A page whose outline starts at its H1</title></head><body>
<main><h1>The subject of the page</h1><h2>A real section</h2><p>{_page("")}</p></main>
</body></html>"""

# No <main>, no <article>, no nav/header/footer, no class a default rule knows:
# nothing in this document places any of its headings.
_UNPLACEABLE_PAGE = f"""<html><head><title>A page that names no region at all</title></head><body>
<div class="wrap"><h1>The subject</h1><h2>A section</h2><p>{_page("")}</p></div>
</body></html>"""


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
    return ctx


def _fired(ctx) -> dict[str, set[str]]:
    out: dict[str, set[str]] = {}
    for issue in ctx.issues:
        out.setdefault(issue.check, set()).add(issue.target_url)
    return out


def _details(ctx, check_id, url):
    return next(i.details for i in ctx.issues if i.check == check_id and i.target_url == url)


def test_heading_before_h1_fires_on_the_masthead_page_and_not_on_the_clean_one():
    ctx = _run_crawl(
        {
            "https://example.com/bad": _FakeResponse(_BAD_PAGE),
            "https://example.com/clean": _FakeResponse(_CLEAN_PAGE),
        }
    )
    fired = _fired(ctx)
    assert "https://example.com/bad" in fired.get("HEADING_BEFORE_H1", set())
    assert "https://example.com/clean" not in fired.get("HEADING_BEFORE_H1", set())
    evidence = _details(ctx, "HEADING_BEFORE_H1", "https://example.com/bad")
    assert evidence["count"] == 2
    assert evidence["levels"] == ["h2"]
    assert evidence["first_texts"] == ["Magazine One", "Magazine Two"]


def test_heading_in_page_chrome_fires_on_the_masthead_page_and_not_on_the_clean_one():
    ctx = _run_crawl(
        {
            "https://example.com/bad": _FakeResponse(_BAD_PAGE),
            "https://example.com/clean": _FakeResponse(_CLEAN_PAGE),
        }
    )
    fired = _fired(ctx)
    assert "https://example.com/bad" in fired.get("HEADING_IN_PAGE_CHROME", set())
    assert "https://example.com/clean" not in fired.get("HEADING_IN_PAGE_CHROME", set())
    evidence = _details(ctx, "HEADING_IN_PAGE_CHROME", "https://example.com/bad")
    assert evidence["count"] == 3
    assert evidence["regions"] == ["footer", "header"]
    assert evidence["first_headings"][0] == {
        "region": "header",
        "level": 2,
        "text": "Magazine One",
    }


def test_heading_skip_fires_only_for_a_content_level_jump():
    skipped = _CLEAN_PAGE.replace(
        "<h2>A real section</h2>", "<h2>A real section</h2><h4>A skipped subsection</h4>"
    )
    chrome_only = _CLEAN_PAGE.replace(
        "<main>", "<header><h2>Menu</h2><h4>Menu group</h4></header><main>"
    )
    ctx = _run_crawl(
        {
            "https://example.com/skipped": _FakeResponse(skipped),
            "https://example.com/chrome-only": _FakeResponse(chrome_only),
        }
    )
    fired = _fired(ctx)
    assert "https://example.com/skipped" in fired.get("HEADING_SKIP", set())
    assert "https://example.com/chrome-only" not in fired.get("HEADING_SKIP", set())
    assert _details(ctx, "HEADING_SKIP", "https://example.com/skipped") == {
        "count": 1,
        "first_jumps": [
            {
                "from_level": 2,
                "from_text": "A real section",
                "to_level": 4,
                "to_text": "A skipped subsection",
            }
        ],
    }


def test_heading_skip_does_not_flag_a_legitimate_level_drop():
    html = _CLEAN_PAGE.replace(
        "<h2>A real section</h2>", "<h2>A real section</h2><h3>A detail</h3><h2>Next section</h2>"
    )
    ctx = _run_crawl({"https://example.com/clean": _FakeResponse(html)})
    assert "HEADING_SKIP" not in _fired(ctx)


def test_heading_skip_fires_on_the_issue_1545_fixture_and_nothing_else():
    """#1545 acceptance: h1 -> h3 and h2 -> h4 are exactly the flagged URLs."""
    h1_to_h3 = _CLEAN_PAGE.replace(
        "<h2>A real section</h2>", "<h3>A subsection with no parent</h3>"
    )
    h2_to_h4 = _CLEAN_PAGE.replace(
        "<h2>A real section</h2>", "<h2>A real section</h2><h4>A skipped subsection</h4>"
    )
    ctx = _run_crawl(
        {
            "https://example.com/jump-from-h1": _FakeResponse(h1_to_h3),
            "https://example.com/jump-from-h2": _FakeResponse(h2_to_h4),
            "https://example.com/clean": _FakeResponse(_CLEAN_PAGE),
        }
    )
    assert _fired(ctx).get("HEADING_SKIP", set()) == {
        "https://example.com/jump-from-h1",
        "https://example.com/jump-from-h2",
    }


def test_heading_skip_fires_on_a_jump_back_up_past_an_open_section():
    """#1545: H4 -> H2 steps past the open H3 section; one-level returns stay clean."""
    back_up = _CLEAN_PAGE.replace(
        "<h2>A real section</h2>",
        "<h2>A real section</h2><h3>A detail</h3><h4>Another detail</h4><h2>Next section</h2>",
    )
    ctx = _run_crawl({"https://example.com/back-up": _FakeResponse(back_up)})
    assert _details(ctx, "HEADING_SKIP", "https://example.com/back-up") == {
        "count": 1,
        "first_jumps": [
            {
                "from_level": 4,
                "from_text": "Another detail",
                "to_level": 2,
                "to_text": "Next section",
            }
        ],
    }


def test_one_finding_per_page_rather_than_one_per_chrome_heading():
    ctx = _run_crawl({"https://example.com/bad": _FakeResponse(_BAD_PAGE)})
    assert len([i for i in ctx.issues if i.check == "HEADING_IN_PAGE_CHROME"]) == 1


def test_a_page_whose_region_cannot_be_determined_is_named_as_unevaluated():
    """The repository's central rule: unmeasured is neither clean nor a defect.
    The page below offers no landmark and matches no position rule, so its
    headings cannot be placed -- the check has to say so by name."""
    ctx = _run_crawl({"https://example.com/unplaceable": _FakeResponse(_UNPLACEABLE_PAGE)})
    fired = _fired(ctx)
    assert "https://example.com/unplaceable" not in fired.get("HEADING_IN_PAGE_CHROME", set())
    skipped = {s.id: s.reason for s in ctx.skipped}
    assert "HEADING_IN_PAGE_CHROME" in skipped
    assert "no content landmark and no matching position rule" in skipped["HEADING_IN_PAGE_CHROME"]
    # Order needs no region, so that half of the outline is still answered.
    assert "HEADING_BEFORE_H1" not in skipped


def test_heading_outline_checks_skip_honestly_on_a_plain_sf_export(result):
    """``result`` (conftest.py) is a real-shaped SF export, which carries no
    outline column at all -- neither check may read that as a clean page."""
    skipped = {s.id: s.reason for s in result.skipped}
    fired = {i.check for i in result.issues}
    for check_id in ("HEADING_BEFORE_H1", "HEADING_IN_PAGE_CHROME", "HEADING_SKIP"):
        assert check_id in skipped
        assert "native crawl only" in skipped[check_id]
        assert check_id not in fired


# -- storage ------------------------------------------------------------------


def test_the_stored_outline_survives_an_import_unchanged(legacy_run, tmp_path):
    pages = legacy_run / "pages.jsonl"
    rows = [json.loads(line) for line in pages.read_text().splitlines()]
    rows[0]["heading_outline"] = [
        {"level": 2, "text": "Menu", "region": "nav"},
        {"level": 1, "text": "Home", "region": "content"},
    ]
    pages.write_text("".join(json.dumps(row) + "\n" for row in rows))
    out = import_run(legacy_run, tmp_path / "scan.sqlite", producer_build=BUILD)
    con = open_scan(out)
    try:
        stored = con.execute(
            "SELECT heading_outline_json FROM pages WHERE page_ordinal=0"
        ).fetchone()[0]
        assert json.loads(stored) == rows[0]["heading_outline"]
    finally:
        con.close()


@pytest.mark.parametrize(
    "outline",
    [
        [{"level": 7, "text": "Out of range", "region": "nav"}],
        [{"level": 1, "text": "No region key"}],
        [{"level": "1", "text": "Level as text", "region": "nav"}],
        "not a list at all",
    ],
)
def test_an_outline_that_is_not_level_text_region_objects_is_refused(legacy_run, tmp_path, outline):
    pages = legacy_run / "pages.jsonl"
    rows = [json.loads(line) for line in pages.read_text().splitlines()]
    rows[0]["heading_outline"] = outline
    pages.write_text("".join(json.dumps(row) + "\n" for row in rows))
    with pytest.raises(ScanError, match="heading_outline"):
        import_run(legacy_run, tmp_path / "bad.sqlite", producer_build=BUILD)
