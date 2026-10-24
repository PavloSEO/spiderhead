"""Sitemap extension parsing and page/attachment separation (#1560).

A real Yoast sitemap carrying 1,283 ``<loc>`` entries and 1,130 ``<image:loc>``
attachments was compared as if the images did not exist, and images the crawl
found were reported as "not in sitemap" even though the sitemap declared them
for their own pages. These tests pin the other behaviour: extensions are parsed
and retained, pages and attachments are counted separately, and the comparison
knows an ``image:loc`` declaration is sitemap membership.
"""

from __future__ import annotations

import httpx

from seohead.checks import sitemap as S
from seohead.crawl.reconcile import reconcile_sitemap

NS = 'xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"'
IMAGE_NS = "http://www.google.com/schemas/sitemap-image/1.1"
NEWS_NS = "http://www.google.com/schemas/sitemap-news/0.9"
VIDEO_NS = "http://www.google.com/schemas/sitemap-video/1.1"
XHTML_NS = "http://www.w3.org/1999/xhtml"


def _yoast_urlset() -> bytes:
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<urlset {NS}
        xmlns:image="{IMAGE_NS}" xmlns:news="{NEWS_NS}"
        xmlns:video="{VIDEO_NS}" xmlns:xhtml="{XHTML_NS}">
  <url>
    <loc>https://example.com/product</loc>
    <lastmod>2026-10-01T12:30:45+03:00</lastmod>
    <image:image>
      <image:loc>/img/photo.webp</image:loc>
      <image:title>Photo</image:title>
      <image:caption>A caption</image:caption>
      <image:geo_location>Kyiv, Ukraine</image:geo_location>
      <image:license>https://example.com/license</image:license>
    </image:image>
    <image:image><image:loc>https://example.com/img/second.jpg</image:loc></image:image>
    <news:news>
      <news:publication><news:name>Example Times</news:name></news:publication>
      <news:publication_date>2026-09-30T08:00:00Z</news:publication_date>
      <news:title>Product launched</news:title>
    </news:news>
    <video:video>
      <video:thumbnail_loc>https://example.com/thumb.jpg</video:thumbnail_loc>
      <video:title>Demo</video:title>
      <video:duration>120</video:duration>
      <video:family_friendly>yes</video:family_friendly>
    </video:video>
    <xhtml:link rel="alternate" hreflang="de" href="https://example.com/de/product"/>
    <xhtml:link rel="alternate" hreflang="en" href="/en/product"/>
  </url>
  <url><loc>https://example.com/plain</loc></url>
</urlset>""".encode()


# ── parse_sitemap retains extensions inside <url> ───────────────────────────


def test_image_extension_fields_are_retained_and_relative_loc_resolves():
    urls = S.parse_sitemap(_yoast_urlset(), "https://example.com/sitemap.xml")["urls"]
    images = urls[0]["images"]
    assert [image["loc"] for image in images] == [
        "https://example.com/img/photo.webp",
        "https://example.com/img/second.jpg",
    ]
    assert images[0]["title"] == "Photo"
    assert images[0]["caption"] == "A caption"
    assert images[0]["geo_location"] == "Kyiv, Ukraine"
    assert images[0]["license"] == "https://example.com/license"


def test_news_extension_reads_the_nested_publication_name():
    urls = S.parse_sitemap(_yoast_urlset(), "https://example.com/sitemap.xml")["urls"]
    assert urls[0]["news"] == {
        "name": "Example Times",
        "publication_date": "2026-09-30T08:00:00Z",
        "title": "Product launched",
    }


def test_video_extension_keeps_the_simple_child_tags():
    urls = S.parse_sitemap(_yoast_urlset(), "https://example.com/sitemap.xml")["urls"]
    assert urls[0]["videos"] == [
        {
            "thumbnail_loc": "https://example.com/thumb.jpg",
            "title": "Demo",
            "duration": "120",
            "family_friendly": "yes",
        }
    ]


def test_hreflang_alternates_resolve_relative_hrefs():
    urls = S.parse_sitemap(_yoast_urlset(), "https://example.com/sitemap.xml")["urls"]
    assert urls[0]["alternates"] == [
        {"hreflang": "de", "href": "https://example.com/de/product"},
        {"hreflang": "en", "href": "https://example.com/en/product"},
    ]


def test_entries_without_extensions_keep_the_exact_base_shape():
    urls = S.parse_sitemap(_yoast_urlset(), "https://example.com/sitemap.xml")["urls"]
    assert urls[1] == {
        "loc": "https://example.com/plain",
        "lastmod": None,
        "changefreq": None,
        "priority": None,
    }


def test_non_standard_and_missing_namespace_prefixes_still_parse():
    prefixed = (
        f'<si:urlset xmlns:si="http://www.sitemaps.org/schemas/sitemap/0.9" '
        f'xmlns:im="{IMAGE_NS}">'
        "<si:url><si:loc>https://example.com/x</si:loc>"
        "<im:image><im:loc>https://example.com/x.png</im:loc></im:image>"
        "</si:url></si:urlset>"
    ).encode()
    assert S.parse_sitemap(prefixed, "https://example.com/")["urls"][0]["images"] == [
        {"loc": "https://example.com/x.png"}
    ]
    # An undeclared prefix is invalid XML, but generators emit it; the recovering
    # parser keeps the raw ``prefix:name`` tag and the local-name match survives.
    undeclared = (
        b"<urlset><url><loc>https://example.com/y</loc>"
        b"<image:image><image:loc>https://example.com/y.webp</image:loc></image:image>"
        b"</url></urlset>"
    )
    assert S.parse_sitemap(undeclared, "https://example.com/")["urls"][0]["images"] == [
        {"loc": "https://example.com/y.webp"}
    ]


# ── crawl(): separate page and attachment counters ──────────────────────────


def _install_fake_transport(tree: dict[str, bytes], monkeypatch) -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        body = tree.get(str(request.url))
        if body is None:
            return httpx.Response(404)
        return httpx.Response(200, content=body, headers={"content-type": "application/xml"})

    def fake_http_client(*_args, **_kwargs):
        return httpx.Client(transport=httpx.MockTransport(handler), follow_redirects=True), True

    monkeypatch.setattr(S, "http_client", fake_http_client)


def test_crawl_counts_pages_and_attachments_separately(monkeypatch):
    _install_fake_transport({"https://example.com/sitemap.xml": _yoast_urlset()}, monkeypatch)

    result = S.crawl("https://example.com/sitemap.xml", concurrency=1)

    # Two pages, two images, one news block, one video — never one merged number.
    assert result["loc_entries"] == 2
    assert result["count"] == 2
    assert result["image_entries"] == 2
    assert result["news_entries"] == 1
    assert result["video_entries"] == 1
    assert result["urls"][0]["images"][0]["loc"] == "https://example.com/img/photo.webp"


def test_streaming_sink_receives_the_extension_fields(monkeypatch):
    _install_fake_transport({"https://example.com/sitemap.xml": _yoast_urlset()}, monkeypatch)
    entries: list[dict] = []

    result = S.crawl("https://example.com/sitemap.xml", concurrency=1, sink=entries.append)

    assert result["urls"] == []
    assert result["image_entries"] == 2
    assert entries[0]["loc"] == "https://example.com/product"
    assert [image["loc"] for image in entries[0]["images"]] == [
        "https://example.com/img/photo.webp",
        "https://example.com/img/second.jpg",
    ]
    assert entries[0]["news"]["name"] == "Example Times"
    # The plain entry keeps the five base keys only.
    assert set(entries[1]) == {"loc", "loc_normalized", "lastmod", "changefreq", "priority"}


def test_a_sitemap_without_extensions_reports_zero_attachments(monkeypatch):
    plain = f"<urlset {NS}><url><loc>https://example.com/a</loc></url></urlset>".encode()
    _install_fake_transport({"https://example.com/sitemap.xml": plain}, monkeypatch)

    result = S.crawl("https://example.com/sitemap.xml", concurrency=1)

    assert result["loc_entries"] == 1
    assert (result["image_entries"], result["news_entries"], result["video_entries"]) == (0, 0, 0)


# ── sitemapindex nesting of any depth, with a cycle guard ───────────────────


def _index(*locs: str) -> bytes:
    items = "".join(f"<sitemap><loc>{u}</loc></sitemap>" for u in locs)
    return f"<sitemapindex {NS}>{items}</sitemapindex>".encode()


def test_an_index_tree_of_any_depth_is_expanded_to_the_leaves(monkeypatch):
    tree = {
        "https://example.com/l1.xml": _index("https://example.com/l2.xml"),
        "https://example.com/l2.xml": _index("https://example.com/l3.xml"),
        "https://example.com/l3.xml": _index("https://example.com/leaf.xml"),
        "https://example.com/leaf.xml": (
            f"<urlset {NS}><url><loc>https://example.com/deep-page</loc></url></urlset>".encode()
        ),
    }
    _install_fake_transport(tree, monkeypatch)

    result = S.crawl("https://example.com/l1.xml", concurrency=1)

    assert result["ok"] is True
    assert [entry["loc"] for entry in result["urls"]] == ["https://example.com/deep-page"]


def test_an_index_cycle_terminates_instead_of_looping_forever(monkeypatch):
    tree = {
        "https://example.com/a.xml": _index("https://example.com/b.xml"),
        "https://example.com/b.xml": _index(
            "https://example.com/a.xml", "https://example.com/leaf.xml"
        ),
        "https://example.com/leaf.xml": (
            f"<urlset {NS}><url><loc>https://example.com/page</loc></url></urlset>".encode()
        ),
    }
    _install_fake_transport(tree, monkeypatch)

    result = S.crawl("https://example.com/a.xml", concurrency=1)

    assert [entry["loc"] for entry in result["urls"]] == ["https://example.com/page"]
    # Each document was fetched exactly once: a.xml, b.xml, leaf.xml.
    assert len(result["sitemaps"]) == 3


# ── lastmod tolerance ────────────────────────────────────────────────────────


def test_every_w3c_lastmod_precision_survives_verbatim():
    values = [
        "2024-01-01",
        "2024-01-01T10:11:12",
        "2024-01-01T10:11:12Z",
        "2024-01-01T10:11:12+02:00",
        "2024-01-01T10:11:12+0200",
        "2024-01-01T10:11:12.123456Z",
        "2024-01-01 10:11:12",
    ]
    for lastmod in values:
        doc = (
            f"<urlset {NS}><url><loc>https://example.com/a</loc>"
            f"<lastmod>{lastmod}</lastmod></url></urlset>"
        ).encode()
        urls = S.parse_sitemap(doc, "https://example.com/")["urls"]
        assert urls[0]["lastmod"] == lastmod, lastmod


def test_sf_date_parser_accepts_every_w3c_lastmod_precision():
    from seohead.sf.core.sitemap_coverage import _parse_date

    for value, expected in (
        ("2024-01-01", "2024-01-01T00:00:00+00:00"),
        ("2024-01-01T10:11:12", "2024-01-01T10:11:12+00:00"),
        ("2024-01-01T10:11:12Z", "2024-01-01T10:11:12+00:00"),
        ("2024-01-01T10:11:12+02:00", "2024-01-01T10:11:12+02:00"),
    ):
        parsed = _parse_date(value)
        assert parsed is not None and parsed.isoformat() == expected, value
    assert _parse_date("not a date at all") is None
    assert _parse_date(None) is None
    assert _parse_date("") is None


# ── BOM and text-sitemap encoding robustness ────────────────────────────────


def test_a_utf8_bom_does_not_break_the_xml_parse():
    bom_xml = b"\xef\xbb\xbf" + _yoast_urlset()
    parsed = S.parse_sitemap(bom_xml, "https://example.com/sitemap.xml")
    assert parsed["ok"] is True
    assert parsed["type"] == "urlset"
    assert parsed["urls"][0]["images"][0]["loc"] == "https://example.com/img/photo.webp"


def test_a_bom_does_not_become_part_of_the_first_text_sitemap_url():
    body = b"https://example.com/one\n# comment\nhttps://example.com/two\n"
    parsed = S.parse_sitemap(b"\xef\xbb\xbf" + body, "https://example.com/sitemap.txt")
    assert [entry["loc"] for entry in parsed["urls"]] == [
        "https://example.com/one",
        "https://example.com/two",
    ]


# ── reconciliation: an image:loc declaration is sitemap membership ──────────


def test_a_crawled_image_declared_as_image_loc_is_not_reported_as_a_defect():
    declared = ["https://example.com/product"]
    declared_images = ["https://example.com/img/photo.webp"]
    observed = ["https://example.com/product", "https://example.com/img/photo.webp"]

    report = reconcile_sitemap(
        declared,
        observed,
        comparable=["https://example.com/product"],
        declared_assets=declared_images,
    )

    # The image is in the sitemap — as an attachment of its page — so it belongs
    # to no problem bucket and to no "not comparable" bucket either.
    assert report["linked_not_in_sitemap"] == []
    assert report["linked_not_comparable"] == []
    assert report["linked_assets_in_sitemap"] == ["https://example.com/img/photo.webp"]
    assert report["asset_entries_in_sitemap"] == 1


def test_an_undeclared_crawled_image_stays_a_named_non_comparable_fact():
    report = reconcile_sitemap(
        ["https://example.com/product"],
        ["https://example.com/product", "https://example.com/img/unrelated.webp"],
        comparable=["https://example.com/product"],
        declared_assets=["https://example.com/img/photo.webp"],
    )
    assert report["linked_assets_in_sitemap"] == []
    assert report["linked_not_comparable"] == ["https://example.com/img/unrelated.webp"]


def test_omitting_declared_assets_keeps_the_historical_output_shape():
    report = reconcile_sitemap(
        ["https://example.com/a"], ["https://example.com/a", "https://example.com/i.webp"]
    )
    assert "asset_entries_in_sitemap" not in report
    assert "linked_assets_in_sitemap" not in report
    assert report["linked_not_in_sitemap"] == ["https://example.com/i.webp"]


# ── the SF coverage parser counts attachments and matches any prefix ────────


def test_sf_parser_counts_extension_entries_per_page():
    from seohead.sf.core import sitemap_coverage as SF

    out = SF._parse_sitemap_bytes(_yoast_urlset(), "ua", 1, set(), set())
    assert [entry["loc"] for entry in out] == [
        "https://example.com/product",
        "https://example.com/plain",
    ]
    assert out[0]["image_entries"] == 2
    assert out[0]["news_entries"] == 1
    assert out[0]["video_entries"] == 1
    # No attachments, no invented keys.
    assert "image_entries" not in out[1]


def test_sf_parser_matches_elements_under_a_mistyped_namespace_uri():
    from seohead.sf.core import sitemap_coverage as SF

    broken = (
        b'<urlset xmlns="http://sitemaps.org/schemas/sitemap/1.0"'  # wrong URI *and* version
        b' xmlns:image="http://www.google.com/schemas/sitemap-image/1.1">'
        b"<url><loc>https://example.com/a</loc>"
        b"<image:image><image:loc>https://example.com/a.webp</image:loc></image:image>"
        b"</url></urlset>"
    )
    out = SF._parse_sitemap_bytes(broken, "ua", 1, set(), set())
    assert [entry["loc"] for entry in out] == ["https://example.com/a"]
    assert out[0]["image_entries"] == 1


def test_sf_parser_stops_on_a_self_referential_index():
    from seohead.sf.core import sitemap_coverage as SF

    self_index = (
        f"<sitemapindex {NS}><sitemap><loc>https://example.com/loop.xml</loc></sitemap>"
        f"</sitemapindex>"
    ).encode()
    # The seen-set guard must terminate the loop before the URL cap is reached.
    out = SF._parse_sitemap_bytes(
        self_index, "ua", 1, {"https://example.com/loop.xml"}, {"example.com"}
    )
    assert out == []


def test_sf_run_sitemap_reports_extension_counters_as_their_own_line(monkeypatch, tmp_path):
    import csv

    from seohead.sf.config import load_config
    from seohead.sf.core import sitemap_coverage as SF
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports

    with open(tmp_path / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["Address", "Content Type", "Status Code", "Indexability"])
        writer.writerow(["https://example.com/product", "text/html", "200", "Indexable"])
    ctx = AuditContext(load_exports(str(tmp_path)), load_config(None))

    def fake_fetch(url, ua, timeout, retries=2):
        if url == "https://example.com/robots.txt":
            return b"User-agent: *\nSitemap: https://example.com/sitemap.xml\n"
        if url == "https://example.com/sitemap.xml":
            return _yoast_urlset()
        return None

    monkeypatch.setattr(SF, "_fetch", fake_fetch)
    summary = SF.run_sitemap(ctx, sitemap_url="https://example.com/sitemap.xml")

    assert summary["urls_in_sitemap"] == 2
    assert summary["image_entries_in_sitemap"] == 2
    assert summary["news_entries_in_sitemap"] == 1
    assert summary["video_entries_in_sitemap"] == 1


def test_sf_run_sitemap_omits_extension_counters_for_a_plain_sitemap(monkeypatch, tmp_path):
    import csv

    from seohead.sf.config import load_config
    from seohead.sf.core import sitemap_coverage as SF
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import load_exports

    with open(tmp_path / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["Address", "Content Type", "Status Code", "Indexability"])
        writer.writerow(["https://example.com/a", "text/html", "200", "Indexable"])
    ctx = AuditContext(load_exports(str(tmp_path)), load_config(None))
    plain = f"<urlset {NS}><url><loc>https://example.com/a</loc></url></urlset>".encode()

    def fake_fetch(url, ua, timeout, retries=2):
        if url == "https://example.com/robots.txt":
            return b"User-agent: *\nSitemap: https://example.com/sitemap.xml\n"
        if url == "https://example.com/sitemap.xml":
            return plain
        return None

    monkeypatch.setattr(SF, "_fetch", fake_fetch)
    summary = SF.run_sitemap(ctx, sitemap_url="https://example.com/sitemap.xml")

    assert summary["urls_in_sitemap"] == 1
    assert "image_entries_in_sitemap" not in summary
    assert "news_entries_in_sitemap" not in summary
    assert "video_entries_in_sitemap" not in summary
