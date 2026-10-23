"""List mode: fetch an explicit URL list, project it, audit it.

Network-free throughout — every response is supplied by a fake fetcher.
"""

import json
from pathlib import Path

import pytest

from seohead.crawl.collect import collect_urls as _collect_urls
from seohead.crawl.evidence import UNAVAILABLE_FRAMES, build_evidence
from seohead.crawl.throttle import Throttle


def collect_urls(urls, **kw):
    """Never sleep for real in tests; back-off behaviour is asserted directly."""
    kw.setdefault("sleeper", lambda _seconds: None)
    return _collect_urls(urls, **kw)


HTML = """<html><head><base href="https://example.com/">
<title>Catalog</title><meta name="description" content="d">
<link rel="canonical" href="catalog/">
<script type="application/ld+json">{"@type":"Product"}</script>
</head><body><h1>Catalog</h1><a href="catalog/x">x</a></body></html>"""


class FakeResponse:
    def __init__(self, text="", status_code=200, headers=None):
        self.text = text
        self.status_code = status_code
        self.headers = headers or {"content-type": "text/html; charset=utf-8"}


def _fetch(mapping):
    def fetcher(url):
        value = mapping[url]
        if isinstance(value, Exception):
            raise value
        return value

    return fetcher


def test_collects_in_the_order_given_and_deduplicates():
    urls = ["https://example.com/a", "https://example.com/b", "https://example.com/a"]
    mapping = {u: FakeResponse(HTML) for u in urls}
    result = collect_urls(urls, fetcher=_fetch(mapping))
    assert [p.url for p in result.pages] == ["https://example.com/a", "https://example.com/b"]


def test_redirect_resolution_rebinds_credentials_for_each_hop(monkeypatch):
    import seohead.crawl.collect as collect_module

    calls = []

    class Client:
        def get(self, url, *, headers, extensions):
            calls.append((url, headers))
            if url == "https://source.example.test/start":
                return FakeResponse("", 302, {"location": "https://other.example.test/landing"})
            return FakeResponse("<html><title>landing</title></html>")

        def close(self):
            pass

    monkeypatch.setenv("SEOHEAD_LIST_TOKEN", "secret")
    monkeypatch.setattr(collect_module, "validate_url", lambda url: url)
    monkeypatch.setattr(collect_module, "pinned_target", lambda url: (url, {}, {}))
    monkeypatch.setattr(collect_module, "http_client", lambda *_args, **_kwargs: (Client(), False))

    collect_urls(
        ["https://source.example.test/start"],
        credential_headers=[
            {
                "host": "source.example.test",
                "headers": {"Authorization": "env:SEOHEAD_LIST_TOKEN"},
            }
        ],
        extra_request_headers={"Accept-Language": "en"},
        resolve_redirect_destination=True,
    )

    assert calls[0][1]["Authorization"] == "secret"
    assert calls[1][1]["Accept-Language"] == "en"
    assert "Authorization" not in calls[1][1]


def test_parses_against_the_document_base(tmp_path):
    """The <base href> fix must hold here too, or list mode invents 404s."""
    result = collect_urls(
        ["https://example.com/section/page"],
        fetcher=_fetch({"https://example.com/section/page": FakeResponse(HTML)}),
    )
    page = result.pages[0]
    assert page.canonical == "https://example.com/catalog/"
    assert page.title == "Catalog"
    assert page.h1 == "Catalog"


def test_reports_json_ld_found_but_unparsed_rather_than_absent():
    """A malformed block must not be reported as "no structured data"."""
    broken = '<html><head><script type="application/ld+json">{ /* c */ }</script></head><body></body></html>'
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(broken)})
    )
    page = result.pages[0]
    assert page.jsonld_blocks_found == 1
    assert page.jsonld_blocks_parsed == 0


def test_rows_are_written_as_they_are_collected(tmp_path):
    """An interrupted run must leave behind what it already had."""
    out = tmp_path / "pages.jsonl"
    urls = [f"https://example.com/{i}" for i in range(3)]
    collect_urls(urls, fetcher=_fetch({u: FakeResponse(HTML) for u in urls}), out_path=str(out))
    lines = [json.loads(line) for line in out.read_text().splitlines()]
    assert [row["url"] for row in lines] == urls


def test_a_timeout_backs_off_instead_of_retrying_immediately():
    urls = ["https://example.com/a", "https://example.com/b"]
    mapping = {urls[0]: TimeoutError("read timed out"), urls[1]: FakeResponse(HTML)}
    result = collect_urls(urls, fetcher=_fetch(mapping))
    assert "timed out" in result.pages[0].error


def test_repeated_timeouts_stop_the_run_and_mark_it_partial():
    urls = [f"https://example.com/{i}" for i in range(8)]
    mapping = {u: TimeoutError("connection timed out") for u in urls}
    result = collect_urls(urls, fetcher=_fetch(mapping))
    assert result.partial is True
    assert "timeouts" in result.stopped_reason
    assert len(result.pages) < len(urls), "must stop rather than walk the whole list"


def test_repeated_connection_failures_stop_the_run_and_mark_it_partial():
    """#132: a fetcher that always raises a connection-level failure — never a
    response, never a message containing "timeout" — used to be invisible to the
    breaker: every one of 50 URLs got attempted and the run reported itself as a
    plain "finished" success. Classification by exception type must catch this."""
    urls = [f"https://example.com/{i}" for i in range(50)]
    mapping = {u: ConnectionResetError("Connection reset by peer") for u in urls}
    result = collect_urls(urls, fetcher=_fetch(mapping))
    assert result.partial is True
    assert result.finish_reason == "errors"
    assert len(result.pages) < len(urls), "must stop rather than walk the whole list"


def test_a_dns_failure_stops_the_run_the_same_way():
    """The other example the issue names: an OSError that carries no HTTP response
    and no "timeout" in its message either — socket.gaierror is exactly this."""
    import socket

    urls = [f"https://example.com/{i}" for i in range(8)]
    mapping = {u: socket.gaierror("Name or service not known") for u in urls}
    result = collect_urls(urls, fetcher=_fetch(mapping))
    assert result.partial is True
    assert result.finish_reason == "errors"


def test_url_limit_marks_the_result_partial():
    urls = [f"https://example.com/{i}" for i in range(5)]
    result = collect_urls(urls, max_urls=2, fetcher=_fetch({u: FakeResponse(HTML) for u in urls}))
    assert len(result.pages) == 2
    assert result.partial is True
    assert result.finish_reason == "url_limit"


def test_a_normal_run_reports_finished_with_no_partial_flag():
    urls = [f"https://example.com/{i}" for i in range(3)]
    result = collect_urls(urls, fetcher=_fetch({u: FakeResponse(HTML) for u in urls}))
    assert result.finish_reason == "finished"
    assert result.partial is False


def test_max_seconds_stops_the_run_with_a_duration_finish_reason():
    urls = [f"https://example.com/{i}" for i in range(10)]
    ticking = {"t": 0.0}

    def fake_clock():
        ticking["t"] += 2
        return ticking["t"]

    result = collect_urls(
        urls,
        max_seconds=5,
        clock=fake_clock,
        fetcher=_fetch({u: FakeResponse(HTML) for u in urls}),
    )
    assert result.finish_reason == "duration_limit"
    assert result.partial is True
    assert len(result.pages) < len(urls)


def test_an_oversized_response_is_not_reported_as_unreachable():
    big = "<html><body>" + ("x" * (6 * 1024 * 1024)) + "</body></html>"
    result = collect_urls(
        ["https://example.com/big"],
        fetcher=_fetch({"https://example.com/big": FakeResponse(big)}),
    )
    page = result.pages[0]
    assert page.status_code == 200
    assert "too large" in page.error


# ── throttle ────────────────────────────────────────────────────────────────


def test_latency_widens_the_delay():
    t = Throttle()
    t.record_response(1.2, ok=True)
    first = t.delay
    t.record_response(16.4, ok=True)
    assert t.delay > first


def test_a_fast_error_never_reduces_the_delay():
    t = Throttle()
    t.record_response(10.0, ok=True)
    before = t.delay
    t.record_response(0.01, ok=False)
    assert t.delay >= before


def test_a_timeout_is_the_strongest_signal():
    t = Throttle(min_delay=0.1)
    t.record_response(1.0, ok=True)
    before = t.delay
    t.record_timeout()
    assert t.delay > before * 2


def test_the_delay_is_bounded():
    t = Throttle(min_delay=1.0)
    for _ in range(20):
        t.record_timeout()
    assert t.delay <= 60.0


# ── projection ──────────────────────────────────────────────────────────────


def test_projection_declares_what_list_mode_cannot_measure():
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(HTML)})
    )
    evidence = build_evidence(result)
    assert evidence["found"] == ["internal_all"]
    assert set(evidence["missing"]) == set(UNAVAILABLE_FRAMES)
    assert "Closest Similarity Match" in evidence["unmeasured_columns"]


def test_projection_uses_the_headers_the_analyzer_resolves_by():
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(HTML)})
    )
    frame = build_evidence(result)["frames"]["internal_all"]
    for column in ("Address", "Status Code", "Title 1", "H1-1", "Indexability"):
        assert column in frame.columns


@pytest.mark.parametrize(
    "status,expected",
    [(200, "Indexable"), (301, "Non-Indexable"), (404, "Non-Indexable"), (500, "Non-Indexable")],
)
def test_indexability_follows_the_status(status, expected):
    body = "<html><head><title>t</title></head><body></body></html>"
    result = collect_urls(
        ["https://example.com/"],
        fetcher=_fetch({"https://example.com/": FakeResponse(body, status_code=status)}),
    )
    frame = build_evidence(result)["frames"]["internal_all"]
    assert frame.iloc[0]["Indexability"] == expected


# ── end to end through the analyzer ─────────────────────────────────────────


def test_a_collected_list_audits_and_declares_its_gaps():
    """The projection must reach a schema-valid audit with honest skips."""
    import json

    import jsonschema

    from seohead.sf.config import load_config
    from seohead.sf.core.aggregate import aggregate
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.rules import run_rules

    urls = [f"https://example.com/{n}" for n in range(3)]
    result = collect_urls(urls, fetcher=_fetch({u: FakeResponse(HTML) for u in urls}))
    evidence = build_evidence(result)

    exports = LoadedExports()
    exports.frames.update(evidence["frames"])
    exports.found = list(evidence["found"])
    exports.missing = list(evidence["missing"])

    ctx = AuditContext(exports, load_config(None))
    ctx.skip_unsupported(set(exports.frames))
    run_rules(ctx)
    audit = aggregate(
        ctx,
        {"input_mode": "crawl-list", "generated_at": "2026-09-03T00:00:00Z"},
        {},
        {},
    ).to_json()

    schema_path = Path("seohead/sf/schema/audit.schema.json")
    schema = json.loads(schema_path.read_text(encoding="utf-8"))
    jsonschema.validate(json.loads(json.dumps(audit)), schema)
    assert audit["summary"]["totals"]["urls_crawled"] == 3
    skipped = {s["id"] for s in audit["run"]["checks_skipped"]}
    assert skipped, "a projection with declared gaps must skip something"
    assert audit["summary"]["check_coverage"]["checks_silent"] >= 0


def test_the_collector_never_imports_the_analyzer():
    """The boundary is a gate, not a convention."""
    import re
    from pathlib import Path

    forbidden = re.compile(r"^\s*(from|import)\s+seohead\.(sf|servers|cli)\b", re.M)
    for path in Path("seohead/crawl").glob("*.py"):
        assert not forbidden.search(path.read_text(encoding="utf-8")), path


def test_the_analyzer_never_imports_the_collector():
    import re
    from pathlib import Path

    forbidden = re.compile(r"^\s*(from|import)\s+seohead\.crawl\b", re.M)
    for path in Path("seohead/sf").rglob("*.py"):
        assert not forbidden.search(path.read_text(encoding="utf-8")), path


def test_json_ld_is_counted_by_tag_not_by_substring():
    """A hydration payload that echoes the media type must not inflate the count.

    One real block on a Next.js page was counted twice, which across a crawl
    reported "found 408, parsed 200" for a site whose structured data is fine.
    """
    from seohead.checks.parser import parse_html
    from seohead.crawl.collect import _jsonld_counts

    html = (
        '<html><head><script id="s" type="application/ld+json">{"@type":"Thing"}</script>'
        '</head><body><script>self.__payload="\\"type\\":\\"application/ld+json\\""</script>'
        "</body></html>"
    )
    assert _jsonld_counts(html, parse_html(html, "https://example.com/")) == (1, 1)


def test_json_ld_array_block_is_one_parsed_block():
    from seohead.checks.parser import parse_html
    from seohead.crawl.collect import _jsonld_counts

    html = (
        '<html><head><script type="application/ld+json">'
        '[{"@type":"Thing"},{"@type":"Thing"}]'
        "</script></head></html>"
    )
    assert _jsonld_counts(html, parse_html(html, "https://example.com/")) == (1, 1)


def test_a_malformed_block_is_found_but_not_parsed():
    from seohead.checks.parser import parse_html
    from seohead.crawl.collect import _jsonld_counts

    html = '<html><head><script type="application/ld+json">{ /* comment */ }</script></head></html>'
    assert _jsonld_counts(html, parse_html(html, "https://example.com/")) == (1, 0)


def test_a_redirect_is_observed_not_followed():
    """A crawler must see the 3xx, not be moved by it.

    With follow_redirects on, a 301 was recorded as a 200 carrying the target's
    title and body, the Location was never seen, and the old and new URL became
    duplicates of each other.
    """
    resp = FakeResponse(
        "", status_code=301, headers={"location": "/new", "content-type": "text/html"}
    )
    result = collect_urls(
        ["https://example.com/old"], fetcher=_fetch({"https://example.com/old": resp})
    )
    page = result.pages[0]
    assert page.status_code == 301
    assert page.redirect_url == "https://example.com/new", "relative Location must be resolved"


def test_text_ratio_is_a_percentage_matching_the_analyzer_threshold():
    """The threshold is a percentage; emitting a fraction fired on every page."""
    body = "<html><head><title>t</title></head><body>" + ("word " * 300) + "</body></html>"
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(body)})
    )
    ratio = result.pages[0].text_ratio
    assert ratio > 1, f"expected a percentage, got {ratio} which reads as a fraction"
    assert ratio <= 100


# #1412 — the ratio's denominator is the markup without script/style blocks, so a
# text-heavy framework page with a large inline payload is not a low-text page.
def test_text_ratio_ignores_inline_script_and_style_bytes():
    words = "word " * 600
    body = (
        "<html><head><title>t</title>"
        f"<style>{'body{{color:red}}' * 3000}</style>"  # ~42 KB of inline CSS
        f"<script>{'/*hydration data*/' * 5000}</script>"  # ~100 KB of inline code
        "<script type=\"application/ld+json\">{\"@type\":\"WebPage\"}</script>"
        "</head><body><p>" + words + "</p></body></html>"
    )
    assert len(body.encode()) > 100_000, "fixture must be genuinely script-dominated"
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(body)})
    )
    ratio = result.pages[0].text_ratio
    assert ratio >= 10, f"a 600-word page over stripped markup must clear the 10% line, got {ratio}"


def test_text_ratio_stays_low_for_a_genuinely_thin_page():
    """Heavy markup around a few words keeps a sub-threshold ratio after stripping."""
    shell = "".join(
        f'<div class="col-md-4 item-{n}" data-track="impressions"><span>&nbsp;</span></div>'
        for n in range(80)
    )
    body = f"<html><head><title>t</title></head><body><nav>{shell}</nav><p>hello world</p></body></html>"
    result = collect_urls(
        ["https://example.com/"], fetcher=_fetch({"https://example.com/": FakeResponse(body)})
    )
    assert result.pages[0].text_ratio < 10


# ── robots.txt in list mode, and the redirect chain (issue #21) ──────────────


def test_list_mode_reads_robots_per_host_and_records_what_it_blocked():
    """List mode has no single site to resolve robots.txt against up front: every URL in
    the list is independent and may live on a different host. Each host's policy is fetched
    as that host is first encountered, and what it disallows is recorded even under a policy
    that still fetches, so "this would be blocked" stays visible rather than only showing up
    as a page that silently went missing."""
    responses = {
        "https://a.example/robots.txt": FakeResponse(
            "User-agent: *\nDisallow: /private/\n", headers={"content-type": "text/plain"}
        ),
        "https://b.example/robots.txt": FakeResponse(
            "User-agent: *\n", headers={"content-type": "text/plain"}
        ),
        "https://a.example/public": FakeResponse(HTML),
        "https://a.example/private/x": FakeResponse(HTML),
        "https://b.example/private/x": FakeResponse(HTML),
    }
    result = collect_urls(
        [
            "https://a.example/public",
            "https://a.example/private/x",
            "https://b.example/private/x",
        ],
        fetcher=_fetch(responses),
        min_delay=0,
        robots_policy="respect",
    )

    fetched = {page.url for page in result.pages}
    # Same path, two hosts, two different policies — so the answer must be per host.
    assert "https://a.example/private/x" not in fetched
    assert "https://b.example/private/x" in fetched
    assert "https://a.example/public" in fetched
    assert result.robots_blocked == ["https://a.example/private/x"]


def test_list_mode_dispatch_gate_paces_per_host_robots_and_cached_page_attempts():
    now = [0.0]
    calls = []

    def fetcher(url):
        calls.append((now[0], url))
        if url.endswith("/robots.txt"):
            return FakeResponse("User-agent: *\n", headers={"content-type": "text/plain"})
        return FakeResponse(HTML)

    _collect_urls(
        ["https://a.example/one", "https://a.example/two", "https://b.example/one"],
        fetcher=fetcher,
        min_delay=1.0,
        robots_policy="respect",
        sleeper=lambda seconds: now.__setitem__(0, now[0] + seconds),
        clock=lambda: now[0],
    )
    assert calls == [
        (0.0, "https://a.example/robots.txt"),
        (1.0, "https://a.example/one"),
        (2.0, "https://a.example/two"),
        (3.0, "https://b.example/robots.txt"),
        (4.0, "https://b.example/one"),
    ]


def test_a_robots_txt_that_cannot_be_read_does_not_block_the_whole_list():
    """A single-site crawl can treat an unreadable robots.txt as "stop, we do not know the
    rules". List mode cannot: one unreachable host would then decide the fate of URLs on
    every other host in the list, which are unrelated to it by construction."""
    responses = {
        "https://down.example/robots.txt": ConnectionError("no route to host"),
        "https://down.example/page": FakeResponse(HTML),
    }
    result = collect_urls(
        ["https://down.example/page"],
        fetcher=_fetch(responses),
        min_delay=0,
        robots_policy="respect",
    )

    assert [page.url for page in result.pages] == ["https://down.example/page"]
    assert result.robots_blocked == []


def test_resolving_a_redirect_destination_records_every_hop_and_where_it_landed():
    """A migration audit needs to know where a chain ends, not merely that a hop exists.
    List mode never follows a redirect as link discovery — depth stays 0 — so this is an
    explicit per-URL chain walk, off by default because a plain status check does not need
    the extra requests."""
    responses = {
        "https://example.com/old": FakeResponse(
            "", status_code=301, headers={"location": "https://example.com/mid"}
        ),
        "https://example.com/mid": FakeResponse(
            "", status_code=302, headers={"location": "https://example.com/new"}
        ),
        "https://example.com/new": FakeResponse(HTML),
    }
    result = collect_urls(
        ["https://example.com/old"],
        fetcher=_fetch(responses),
        min_delay=0,
        resolve_redirect_destination=True,
    )

    page = result.pages[0]
    assert page.url == "https://example.com/old"
    assert page.final_url == "https://example.com/new"
    assert [hop["url"] for hop in page.redirect_chain] == [
        "https://example.com/mid",
        "https://example.com/new",
    ]
    assert [hop["status_code"] for hop in page.redirect_chain] == [302, 200]


def test_an_unresolved_redirect_reports_an_empty_chain_rather_than_a_false_destination():
    """Off by default, and the absence has to be legible: an empty chain beside a non-empty
    redirect_url means "nobody followed this", which is a different statement from "this
    redirect resolves to nothing"."""
    responses = {
        "https://example.com/old": FakeResponse(
            "", status_code=301, headers={"location": "https://example.com/new"}
        ),
    }
    result = collect_urls(["https://example.com/old"], fetcher=_fetch(responses), min_delay=0)

    page = result.pages[0]
    assert page.redirect_url == "https://example.com/new"
    assert page.redirect_chain == []
    assert page.final_url == ""


def test_resolving_a_canonical_destination_records_each_hop_without_discovery():
    """Canonical resolution is a per-row audit, not a crawl of canonical targets."""
    responses = {
        "https://example.com/a": FakeResponse(
            '<link rel="canonical" href="https://example.com/b">'
        ),
        "https://example.com/b": FakeResponse(
            '<link rel="canonical" href="https://example.com/c">'
        ),
        "https://example.com/c": FakeResponse(
            '<link rel="canonical" href="https://example.com/c">'
        ),
    }

    result = collect_urls(
        ["https://example.com/a"],
        fetcher=_fetch(responses),
        min_delay=0,
        resolve_canonical_destination=True,
    )

    page = result.pages[0]
    assert [p.url for p in result.pages] == ["https://example.com/a"]
    assert page.final_canonical == "https://example.com/c"
    assert [hop["url"] for hop in page.canonical_chain] == [
        "https://example.com/b",
        "https://example.com/c",
    ]
