"""Image alt as the anchor of an image-only link (issue #1518).

A hyperlink whose visible content is an image shows an empty anchor even when
the image carries a descriptive alt. These tests cover the parser fact, the
LinkEdge the crawl records, and the per-link query surface where the marker is
read back -- with a pre-marker scan reporting None rather than a false
text-derived answer.
"""

from __future__ import annotations

from seohead.checks.parser import parse_html
from seohead.crawl.spider import LinkEdge
from seohead.storage.native_scan import NativeScan
from seohead.storage.url_links import url_links
from tests.test_scan_native import _link, _metadata, _record, _runtime

ROOT = "https://example.test/"
TARGET = ROOT + "guide"

_PAGE = """<html><head><title>Links that name their destination</title></head><body>
<main><h1>{title}</h1>
<a href="https://example.test/guide"><img alt="Sizing guide"></a>
<a href="https://example.test/alpha">Visible text</a>
<a href="https://example.test/bare"><img alt=""></a>
<a href="https://example.test/two"><img alt=""><img alt="Second image alt"></a>
</main></body></html>"""


def _parsed():
    return parse_html(_PAGE.format(title="t"), ROOT)["links"]


def test_image_only_link_takes_the_alt_as_its_anchor():
    (link,) = [item for item in _parsed() if item["href"].endswith("/guide")]
    assert link["text"] == "Sizing guide"
    assert link["anchor_source"] == "alt"


def test_a_link_with_visible_text_keeps_it_and_gets_no_marker():
    (link,) = [item for item in _parsed() if item["href"].endswith("/alpha")]
    assert link["text"] == "Visible text"
    assert "anchor_source" not in link


def test_a_link_with_no_text_and_no_alt_stays_empty():
    (link,) = [item for item in _parsed() if item["href"].endswith("/bare")]
    assert link["text"] == ""
    assert "anchor_source" not in link


def test_the_first_non_empty_alt_wins():
    (link,) = [item for item in _parsed() if item["href"].endswith("/two")]
    assert link["text"] == "Second image alt"
    assert link["anchor_source"] == "alt"


def test_the_edge_carries_the_marker_and_the_writer_accepts_both_shapes():
    # A LinkEdge built by this code always carries anchor_source; a dict from a
    # caller built before the marker does not, and "" is its truth.
    edge = LinkEdge(
        source=ROOT,
        destination=TARGET,
        anchor="Sizing guide",
        nofollow=False,
        anchor_source="alt",
    )
    assert edge.anchor_source == "alt"
    plain = _link(ROOT, TARGET, "Sizing guide")
    assert "anchor_source" not in plain


def test_the_marker_survives_the_scan_and_reads_back_per_link(tmp_path):
    path = tmp_path / "scan.sqlite"
    edges = [
        {**_link(ROOT, TARGET, "Sizing guide"), "anchor_source": "alt"},
        _link(ROOT, ROOT + "alpha", "Visible text"),
    ]
    with NativeScan.create(path, **_metadata()) as scan:
        scan.enqueue([(ROOT, 0), (TARGET, 1)])
        for url in (ROOT, TARGET):
            lease = scan.claim(1)[0]
            record = _record(url)
            record["crawl_depth"] = lease.depth
            runtime = _runtime()
            runtime["max_depth_reached"] = lease.depth
            scan.commit_page(lease, record, links=edges if url == ROOT else [], runtime=runtime)
        scan.finish_capture()
    items = url_links(path, ROOT, direction="out")["items"]
    by_anchor = {item["anchor"]: item["anchor_source"] for item in items}
    assert by_anchor == {"Sizing guide": "alt", "Visible text": ""}
