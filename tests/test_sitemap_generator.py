"""Offline sitemap generation from retained scan evidence and protocol boundaries."""

from __future__ import annotations

import hashlib
import socket
import sqlite3
import xml.etree.ElementTree as ET
from dataclasses import replace

import pytest

from seohead.mcp.scan_sitemaps import generate_scan_sitemaps
from seohead.storage import sitemap_generator as generator
from seohead.storage.native_scan import NativeScan
from tests.test_scan_corpus_write import _event
from tests.test_scan_native import _metadata, _record, _runtime
from tests.test_scan_sql_sitemap import _robots_blocked

ROOT = "https://example.test/"
NS = {"sm": generator.NAMESPACE, "image": generator.IMAGE_NAMESPACE}


@pytest.fixture(autouse=True)
def offline(monkeypatch):
    def deny(*args, **kwargs):
        raise AssertionError("sitemap generation must not access the network")

    monkeypatch.setattr(socket, "getaddrinfo", deny)
    monkeypatch.setattr(socket, "create_connection", deny)


def _scan(tmp_path, records, *, finished=True, format_version="scan.v1"):
    path = tmp_path / "scan.sqlite"
    metadata = _metadata(
        **{
            "storage.format_version": format_version,
            "storage.body_mode": "captured_entity_bytes",
            "storage.max_body_bytes": 2 * 1024 * 1024,
            "storage.max_body_store_bytes": 8 * 1024 * 1024,
        }
    )
    with NativeScan.create(path, format_version=format_version, **metadata) as scan:
        scan.enqueue([(record["url"], 0) for record in records])
        for values in records:
            values = dict(values)
            url = values.pop("url")
            body = values.pop("body", None)
            modified = values.pop("lastmod", None)
            blocked = values.pop("blocked", False)
            record = _record(url)
            record.update(status_code=200, crawl_depth=0)
            record.update(values)
            captures = []
            if body is not None or modified is not None:
                event = _event(
                    requested_url=url,
                    effective_url=url,
                    entity_bytes=body if body is not None else b"<html></html>",
                )
                if modified is not None:
                    event = replace(
                        event,
                        effective_headers=(*event.effective_headers, ("last-modified", modified)),
                    )
                captures = [event]
            scan.commit_page(scan.claim(1)[0], record, captures=captures, runtime=_runtime())
            if blocked:
                _robots_blocked(scan, url)
        if finished:
            assert scan.finish_without_audit()
    return path


def _locations(out, prefix="sitemap"):
    return [
        node.text
        for path in sorted(out.glob(f"{prefix}-[0-9]*.xml"))
        for node in ET.parse(path).findall("sm:url/sm:loc", NS)
    ]


def test_exclusions_escape_and_index_with_immutable_scan(tmp_path):
    records = [
        {"url": ROOT + "?x=1&y=2", "canonical": ROOT + "?x=1&y=2"},
        {"url": ROOT + "gone", "status_code": 404},
        {"url": ROOT + "redirect", "status_code": 301},
        {"url": ROOT + "noindex", "meta_robots": "noindex,follow"},
        {"url": ROOT + "none", "x_robots": "none"},
        {"url": ROOT + "blocked", "blocked": True},
        {"url": ROOT + "canonical", "canonical": "/target"},
        {"url": ROOT + "chain", "final_canonical": ROOT + "target"},
        {"url": ROOT + "trailing", "canonical": ROOT + "trailing/"},
        {"url": ROOT + "exclude"},
        {"url": "https://other.test/"},
    ]
    path = _scan(tmp_path, records)
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT, exclude=[ROOT + "exclude"])
    assert result["ok"], result
    assert _locations(out) == [ROOT + "?x=1&y=2"]
    assert b"&amp;" in (out / "sitemap-0001.xml").read_bytes()
    assert result["exclusions"] == {
        "non_canonical": 3,
        "noindex": 2,
        "not_200": 2,
        "outside_origin": 1,
        "robots_blocked": 1,
        "user_excluded": 1,
    }
    index = ET.parse(result["index"])
    assert [n.text for n in index.findall("sm:sitemap/sm:loc", NS)] == [ROOT + "sitemap-0001.xml"]
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before
    assert not list(tmp_path.glob("scan.sqlite-*"))
    assert result["coverage"]["state"] == "complete"


def test_lastmod_header_and_retained_modified_evidence_only(tmp_path):
    def signals(signal, value):
        return {
            "author": [],
            "article": [],
            "dates": [{"signal": signal, "confidence": "high", "value": value}],
        }

    path = _scan(
        tmp_path,
        [
            {
                "url": ROOT + "header",
                "lastmod": "Wed, 21 Oct 2015 07:28:00 GMT",
                "trust_signals": signals("meta_article_modified", "2026-01-02"),
            },
            {
                "url": ROOT + "modified",
                "lastmod": "invalid",
                "trust_signals": signals("jsonld_date:dateModified", "2026-01-02T13:40:00+03:00"),
            },
            {"url": ROOT + "date", "trust_signals": signals("meta_article_modified", "2026-01-02")},
            {
                "url": ROOT + "published",
                "trust_signals": signals("meta_article_published", "2026-01-02"),
            },
            {
                "url": ROOT + "invalid",
                "trust_signals": signals("meta_article_modified", "2026-02-30"),
            },
            {
                "url": ROOT + "naive",
                "trust_signals": signals("meta_article_modified", "2026-01-02T10:00:00"),
            },
        ],
    )
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert result["ok"], result
    dates = [
        node.findtext("sm:lastmod", namespaces=NS)
        for node in ET.parse(out / "sitemap-0001.xml").findall("sm:url", NS)
    ]
    assert dates == ["2015-10-21T07:28:00Z", "2026-01-02T10:40:00Z", "2026-01-02", None, None, None]
    assert result["coverage"]["lastmod_sources"]["unavailable"] == 3


def test_image_sitemap_uses_complete_html_base_srcset_and_excludes_known_failures(tmp_path):
    html = (
        b'<html><head><base href="/assets/"></head><body>'
        b'<img src="a.jpg"><img src="a.jpg"><picture><source srcset="b.webp 1x, c.webp 2x">'
        b'<img src="https://cdn.example.test/photo.jpg?x=1&amp;y=2"></picture>'
        b'<video><source src="video.mp4"></video><img src="data:image/png;base64,abc">'
        b'<img src="/broken.jpg"><img src="/private.jpg"><img src="/omit.jpg">'
        b'<template><img src="inert.jpg"></template></body></html>'
    )
    path = _scan(
        tmp_path,
        [
            {"url": ROOT + "page", "body": html},
            {"url": ROOT + "broken.jpg", "status_code": 404, "content_type": "image/jpeg"},
            {"url": ROOT + "private.jpg", "blocked": True, "content_type": "image/jpeg"},
            {"url": ROOT + "unretained"},
        ],
    )
    out = tmp_path / "sitemaps"
    result = generate_scan_sitemaps(
        str(path), str(out), ROOT, include_images=True, exclude=[ROOT + "omit.jpg"]
    )
    assert result["ok"], result
    assert _locations(out, "image-sitemap") == [ROOT + "page"]
    images = [
        n.text
        for n in ET.parse(out / "image-sitemap-0001.xml").findall(
            "sm:url/image:image/image:loc", NS
        )
    ]
    assert set(images) == {
        ROOT + "assets/a.jpg",
        ROOT + "assets/b.webp",
        ROOT + "assets/c.webp",
        "https://cdn.example.test/photo.jpg?x=1&y=2",
    }
    assert result["counts"]["images"] == 4
    assert result["coverage"]["images"]["state"] == "partial"
    assert result["coverage"]["images"]["counts"]["excluded_image"] == 2
    assert result["coverage"]["images"]["counts"]["user_excluded_image"] == 1
    assert result["coverage"]["images"]["counts"]["image_response_unavailable"] == 4
    assert len(ET.parse(result["index"]).findall("sm:sitemap", NS)) == 2


def test_unavailable_image_bodies_and_partial_scan_are_explicit(tmp_path):
    path = _scan(tmp_path, [{"url": ROOT}], finished=False)
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT, include_images=True)
    assert result["ok"], result
    assert result["coverage"]["state"] == "partial"
    assert result["coverage"]["images"]["state"] == "unavailable"
    assert not list(out.glob("image-sitemap*"))


def test_image_limit_is_reported_and_noncanonical_pages_have_no_images(tmp_path):
    html = (
        "<html><body>"
        + "".join(f'<img src="/image-{i}.jpg">' for i in range(1001))
        + "</body></html>"
    ).encode()
    path = _scan(
        tmp_path,
        [
            {"url": ROOT, "body": html},
            {
                "url": ROOT + "duplicate",
                "canonical": ROOT,
                "body": b'<img src="only-excluded.jpg">',
            },
        ],
    )
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT, include_images=True)
    assert result["ok"], result
    assert result["counts"]["images"] == 1000
    assert result["coverage"]["images"]["state"] == "partial"
    assert result["coverage"]["images"]["counts"]["image_limit_omitted"] == 1


def test_normalized_duplicate_urls_and_directory_scope(tmp_path):
    path = _scan(
        tmp_path,
        [
            {"url": ROOT + "shop/item"},
            {"url": "https://EXAMPLE.test/shop/item"},
            {"url": ROOT + "outside"},
        ],
    )
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT + "shop/")
    assert result["ok"], result
    assert _locations(out) == [ROOT + "shop/item"]
    assert result["exclusions"] == {"duplicate_url": 1, "outside_sitemap_directory": 1}
    assert (
        ET.parse(result["index"]).findtext("sm:sitemap/sm:loc", namespaces=NS)
        == ROOT + "shop/sitemap-0001.xml"
    )


def test_count_split_at_real_50000_boundary(tmp_path):
    writer = generator._Writer(tmp_path, "sitemap", {})
    try:
        for i in range(50_001):
            writer.add(generator._entry(ROOT + str(i)))
        writer.finish()
        assert [f["urls"] for f in writer.files] == [50_000, 1]
        assert all(f["bytes"] == (tmp_path / f["name"]).stat().st_size for f in writer.files)
        assert len(ET.parse(tmp_path / "sitemap-0001.xml").getroot()) == 50_000
    finally:
        writer.close()


def test_byte_split_counts_utf8_wrapper_and_footer_exactly(tmp_path, monkeypatch):
    entry = generator._entry(ROOT + "?x=1&y=2", images=[ROOT + "photo.jpg"])
    writer = generator._Writer(tmp_path, "image-sitemap", {}, images=True)
    limit = len(writer.header) + 2 * len(entry) + len(writer.footer)
    monkeypatch.setattr(generator, "MAX_BYTES", limit)
    try:
        for _ in range(3):
            writer.add(entry)
        writer.finish()
        assert [f["urls"] for f in writer.files] == [2, 1]
        assert writer.files[0]["bytes"] == limit
        assert all((tmp_path / f["name"]).stat().st_size <= limit for f in writer.files)
    finally:
        writer.close()


def test_real_50mb_boundary_is_enforced_before_50000_urls(tmp_path):
    entry = generator._entry(ROOT + "x" * (2047 - len(ROOT)))
    writer = generator._Writer(tmp_path, "sitemap", {})
    fitting = (generator.MAX_BYTES - len(writer.header) - len(writer.footer)) // len(entry)
    assert fitting < 50_000
    try:
        for _ in range(fitting + 1):
            writer.add(entry)
        writer.finish()
        assert [item["urls"] for item in writer.files] == [fitting, 1]
        for item in writer.files:
            path = tmp_path / item["name"]
            assert path.stat().st_size == item["bytes"] <= 50 * 1024 * 1024
    finally:
        writer.close()


def test_split_output_has_every_part_in_index(tmp_path, monkeypatch):
    path = _scan(tmp_path, [{"url": ROOT + str(i)} for i in range(5)])
    # Three URL parts also fit in the three-entry index.
    monkeypatch.setattr(generator, "MAX_URLS", 3)
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert result["ok"], result
    assert [item["urls"] for item in result["files"]] == [3, 2]
    assert _locations(out) == [ROOT + str(i) for i in range(5)]
    assert [n.text for n in ET.parse(result["index"]).findall("sm:sitemap/sm:loc", NS)] == [
        ROOT + "sitemap-0001.xml",
        ROOT + "sitemap-0002.xml",
    ]


def test_index_limit_failure_cleans_all_parts(tmp_path, monkeypatch):
    path = _scan(tmp_path, [{"url": ROOT}])
    original = generator._entry

    def oversized_index_entry(*args, **kwargs):
        if kwargs.get("tag") == "sitemap":
            return b"x" * generator.MAX_BYTES
        return original(*args, **kwargs)

    monkeypatch.setattr(generator, "_entry", oversized_index_entry)
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert not result["ok"] and "index exceeds" in result["error"]
    assert not out.exists()


@pytest.mark.parametrize(
    "base",
    [
        "/relative/",
        "https://other.test/",
        "https://example.test/path",
        "https://example.test/?q=1",
        "https://user:pass@example.test/",
        "https://example.test/#fragment",
        "https://bad host.test/",
    ],
)
def test_bad_base_fails_without_output(tmp_path, base):
    path = _scan(tmp_path, [{"url": ROOT}])
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), base)
    assert not result["ok"]
    assert not out.exists()


def test_existing_output_preserved_and_partial_write_failure_cleaned(tmp_path, monkeypatch):
    path = _scan(tmp_path, [{"url": ROOT}])
    existing = tmp_path / "existing"
    existing.mkdir()
    (existing / "keep").write_text("user data")
    assert not generator.generate_sitemaps(str(path), str(existing), ROOT)["ok"]
    assert (existing / "keep").read_text() == "user data"
    original = generator._write_file

    def fail_after_write(*args, **kwargs):
        original(*args, **kwargs)
        raise OSError("simulated write failure")

    monkeypatch.setattr(generator, "_write_file", fail_after_write)
    out = tmp_path / "failed"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert not result["ok"] and "simulated" in result["error"]
    assert not out.exists()


def test_no_eligible_urls_and_overlarge_entry_remove_owned_files(tmp_path, monkeypatch):
    path = _scan(tmp_path, [{"url": ROOT, "meta_robots": "noindex"}])
    out = tmp_path / "none"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert not result["ok"] and "no eligible" in result["error"]
    assert not out.exists()
    monkeypatch.setattr(generator, "MAX_BYTES", 10)
    writer = generator._Writer(tmp_path, "sitemap", {})
    with pytest.raises(ValueError, match="one sitemap entry"):
        writer.add(generator._entry(ROOT))
    assert not writer.files


@pytest.mark.parametrize(
    "kwargs",
    [{"exclude": "*"}, {"exclude": [""]}, {"exclude": ["x"] * 101}, {"include_images": "yes"}],
)
def test_invalid_options_fail_before_writing(tmp_path, kwargs):
    out = tmp_path / "sitemaps"
    result = generator.generate_sitemaps("missing.sqlite", str(out), ROOT, **kwargs)
    assert not result["ok"]
    assert not out.exists()


def test_invalid_source_fails_without_creating_scan_or_output(tmp_path):
    path, out = tmp_path / "missing.sqlite", tmp_path / "sitemaps"
    result = generator.generate_sitemaps(str(path), str(out), ROOT)
    assert not result["ok"]
    assert not path.exists() and not out.exists()


def test_image_sitemap_excludes_resource_only_error_and_keeps_unknown_explicit(tmp_path):
    path = _scan(
        tmp_path,
        [
            {
                "url": ROOT + "page",
                "body": b'<html><body><img src="/broken.jpg"><img src="/healthy.jpg"><img src="/unknown.jpg"></body></html>',
            }
        ],
        format_version="scan.v2",
    )
    with sqlite3.connect(path) as con:
        for name, status, state in [
            ("broken.jpg", 404, "failed"),
            ("healthy.jpg", 200, "complete"),
            ("unknown.jpg", None, "failed"),
        ]:
            con.execute(
                "INSERT INTO resource_graph_fetches(resolved_url,state,reason,status_code,content_type,bytes_received,origin_host,redirects,nesting_depth) VALUES(?,?,?,?,?,32,'example.test',0,0)",
                (ROOT + name, state, "fixture", status, "image/jpeg"),
            )
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    out = tmp_path / "sitemaps"
    result = generate_scan_sitemaps(str(path), str(out), ROOT, include_images=True)
    assert result["ok"], result
    images = [
        node.text
        for node in ET.parse(out / "image-sitemap-0001.xml").findall(
            "sm:url/image:image/image:loc", NS
        )
    ]
    assert ROOT + "broken.jpg" not in images
    assert images == [ROOT + "healthy.jpg", ROOT + "unknown.jpg"]
    assert result["coverage"]["images"]["state"] == "partial"
    assert result["coverage"]["images"]["counts"]["resource_non_200_image"] == 1
    assert result["coverage"]["images"]["counts"]["image_response_unavailable"] == 1
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_unfetched_image_responses_are_not_claimed_complete(tmp_path):
    path = _scan(
        tmp_path, [{"url": ROOT, "body": b'<html><body><img src="/unfetched.jpg"></body></html>'}]
    )
    result = generate_scan_sitemaps(
        str(path), str(tmp_path / "sitemaps"), ROOT, include_images=True
    )
    assert result["ok"], result
    assert result["counts"]["images"] == 1
    assert result["coverage"]["images"]["state"] == "partial"
    assert result["coverage"]["images"]["counts"]["image_response_unavailable"] == 1
